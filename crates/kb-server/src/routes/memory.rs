//! `GET /api/memory/recall?q=&scope=all|global|project&project=&limit=`
//! — agent-memory recall. Fans out across the in-scope memory corpora,
//! over-fetches per corpus, then re-ranks globally by
//! `rank-position × salience × recency-decay` (`kb_core::memory::rerank`),
//! dropping superseded/forgotten memories. This is the only endpoint that
//! spans corpora; `kb recall` and the SPA `/memory` view both call it.

use crate::middleware::error_to_problem_json;
use crate::state::{KbContext, KbHandles};
use axum::{
    body::Body,
    extract::{Extension, Path, Query, State},
    http::{Response, StatusCode},
    response::IntoResponse,
    Json,
};
use kb_core::memory::{rerank_with_policy_scored, DecayPolicy, RecallHit, DEFAULT_SALIENCE};
use kb_core::storage::sqlite::CodeRefRow;
use kb_core::types::KbName;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 50;
/// CT-C4 — per-hit cap on `RecallResult::code_hints`. Truncation is always
/// explicit: `code_hints_total` carries the pre-cap distinct count.
const CODE_HINTS_CAP: usize = 5;

#[derive(Debug, Deserialize)]
pub struct Params {
    /// Recall query. Empty/absent → recency timeline (the SPA `/memory`
    /// view's loose query); a non-empty query runs hybrid/BM25 search.
    #[serde(default)]
    pub q: String,
    /// `all` (default) | `global` | `project`.
    #[serde(default = "default_scope")]
    pub scope: String,
    /// Restrict `scope=project` to a named corpus (else the single
    /// project corpus on this daemon).
    pub project: Option<String>,
    pub limit: Option<usize>,
    /// L7 — visibility filter. When set, only return memories whose
    /// V0010 link set contains either the `*` global sentinel or the
    /// named kb. Used by per-kb /memory views, the PreviewInspector
    /// "Related memories" panel, and any caller that wants the
    /// "visible to this kb" slice instead of the full corpus.
    pub for_kb: Option<String>,
    /// CT-B2 (memory-scoping) — csv of kb names widening recall to
    /// memories explicitly linked to any of them, WITHOUT hiding the
    /// unlinked commons. Distinct from `for_kb`'s strict allowlist: a
    /// memory with no V0010 link rows at all is visible under
    /// `visible_to` regardless of its contents (unlinked memories are
    /// visible everywhere); a memory with a non-empty link set is
    /// visible only when that set carries `*` or intersects this csv.
    /// Composes with `for_kb` (AND — both filters must pass). Absent ⇒
    /// no filtering (byte-identical to pre-CT-B2). The `context` route's
    /// memories lane threads its `memory_visible_to` param through here.
    pub visible_to: Option<String>,
    /// RA-recall — bypass the per-corpus decay/salience floor for this
    /// call only (the `rerank` math is unchanged, so #10 holds). The
    /// `/kb-reflect` dream-loop uses it as a dedup ORACLE: the floor
    /// otherwise hides exactly the low-salience / decayed memories a
    /// distiller must see to avoid re-creating a near-duplicate.
    #[serde(default)]
    pub no_floor: bool,
    /// MI-W4.2a — opt-in per-week injection histogram
    /// (`RecallResult::recall_weekly`), an extra ledger fan-out
    /// (`fetch_recall_weekly`) the hot per-turn `kb-recall` hook path never
    /// pays for. The `/memory` SPA view (the row sparkline's only consumer)
    /// sets this; every other caller — including `kb recall` itself —
    /// leaves it off and gets an empty `recall_weekly` on every hit
    /// (absent data, not a zero reading).
    #[serde(default)]
    pub with_weekly: bool,
}

fn default_scope() -> String {
    "all".to_string()
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct RecallResponse {
    pub hits: Vec<RecallResult>,
    pub ms: u64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "RecallHit")
)]
#[derive(Debug, Serialize)]
pub struct RecallResult {
    pub id: String,
    pub kb: String,
    pub title: String,
    /// Absolute on-disk path (what the indexer stored).
    pub path: String,
    /// Source-root-relative path; the SPA builds `/a/<kb>/<rel>` from it.
    pub source_relative: String,
    pub score: f32,
    pub salience: f32,
    /// v0.10 M3 — true when the memory is on its kb's pinned_memories
    /// table. Drives the table-row pin state in the /memory view.
    #[serde(default)]
    pub pinned: bool,
    /// v0.14 T1 — origin Claude Code session id from `<meta
    /// name="kb-session">`. Drives the "from session: …" sub-line on
    /// /memory rows; `None` for memories that pre-date S1 or were
    /// created outside a session (e.g. via curl with `--no-session`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_id: Option<String>,
    /// RA4 — one-line summary distinct from the title, from `<meta
    /// name="kb-summary">`. The SPA /memory view + related-memories popover
    /// render it as a gloss beneath the title. Absent when the memory has
    /// no kb-summary meta.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub summary: Option<String>,
    /// MI-W3.3a — optional CoALA-minimal classification from `<meta
    /// name="kb-memory-type">` (`episodic` | `semantic` | `procedural`).
    /// Absent for the vast majority of the corpus — untyped, not a default.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub memory_type: Option<String>,
    /// MI-W3.4 — write-time trust tag from `<meta name="kb-source">`
    /// (`fetched-web` | `user-dictated` | `agent-inference`). SURFACED
    /// (display only), NEVER a scoring input.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source: Option<String>,
    /// CT-A1 (U3 parse-back) — the `you`/`claude` role this memory was
    /// highlighted under, from `<meta name="kb-author">`. Absent for the
    /// vast majority of the corpus (only highlight-born memories carry
    /// this). SURFACED (display only), NEVER a scoring input.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM,
    /// from `<meta name="kb-source-kb">`. Absent when not highlight-born.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_kb: Option<String>,
    /// CT-A1 — artifact id of that origin artifact, from
    /// `<meta name="kb-source-artifact">`. Absent when not highlight-born.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_artifact: Option<String>,
    /// CT-A1 — the origin selection, from `<meta name="kb-source-anchor">`.
    /// Best-effort parsed `review::Anchor`; absent when not highlight-born
    /// OR the stored JSON fails to parse (never a 500 — see
    /// `parse_source_anchor`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_anchor: Option<kb_core::review::Anchor>,
    /// L7 — true when the memory's link set contains the `*`
    /// sentinel. The SPA renders a ★ chip and skips per-kb chip
    /// rendering when global is on.
    #[serde(default)]
    pub global: bool,
    /// L7 — explicit kb name list (excludes the `*` sentinel), sorted
    /// asc. Drives the chip strip on /memory rows.
    #[serde(default)]
    pub linked_kbs: Vec<String>,
    /// RP-track — best-effort reading state for this artifact: the human's
    /// furthest scroll % (`read_pct`), when they last read it
    /// (`last_read_at`, unix seconds), and the section they stopped in
    /// (`stopped_at`). Absent when there's no recorded visit or the kb has
    /// reading capture off — so the agent learns what the user has actually
    /// consumed without a follow-up call. Distinct from "touched" (what the
    /// agent referenced in a transcript): this is human reading.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub read_pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_read_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub stopped_at: Option<String>,
    /// invariant:10 decomposition — the arithmetic behind `score`, surfaced
    /// (never re-scored) so a scorechip/`--explain` renderer needs no
    /// re-derivation: `score ≈ rel × salience × decay`. Additive + wire-
    /// optional (older SPA bundles ignore unknown fields); `rerank_with_policy`
    /// always computes these for every hit, so in practice they're always
    /// `Some` on a live daemon.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub rank: Option<u32>,
    /// `1/(60+rank)` — the rank-relevance factor.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub rel: Option<f32>,
    /// `exp(-k*age_days)` — the recency-decay factor.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub decay: Option<f32>,
    /// Age in days the `decay` factor was computed against (0.0 when the
    /// memory has no mtime, i.e. undecayed).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub age_days: Option<f32>,
    /// MI-W1.3 — how many times a `kb-recall` hook actually injected this
    /// memory into a captured session, summed across every kb's
    /// `memory_recalls` ledger (invariant #28). DISPLAY enrichment only —
    /// computed AFTER `rerank_with_policy` already ran above; never an
    /// input to `score`/`rank`/`rel`/`decay`. Defaults to 0 (not absent —
    /// "never recalled" is itself meaningful, unlike the reading-progress
    /// fields above, which are genuinely unknown until a visit exists).
    #[serde(default)]
    pub recall_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_recalled_at: Option<i64>,
    /// CT-C5 (V0037) — how many of `recall_count`'s injections a later turn
    /// in the recalling session EXPLICITLY REFERENCED (the memory's id or
    /// title, verbatim) — always `<= recall_count`. A lower bound on
    /// usefulness, not a full one: an agent can act on a recalled fact
    /// without ever naming it, and that reads identically to "unreferenced"
    /// here. Same enrichment pass as `recall_count` (display only, computed
    /// strictly after rerank — never an input to `score`/`rank`/`rel`/
    /// `decay`, and never read by `rerank_with_policy_scored`'s stability
    /// term either, unlike `recall_count` itself).
    #[serde(default)]
    pub recall_used_count: u32,
    /// MI-W2.1 decomposition, split MI-W5.R — the per-corpus normalized
    /// relevance factor `score` was additionally multiplied by. `None` when
    /// `[memory] scoring_v2_relevance` is off; always present (possibly
    /// `1.0`, a no-op) when it's on (**the daemon-wide default**).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub relevance_factor: Option<f32>,
    /// MI-W2.2 decomposition, split MI-W5.R — the stability multiplier
    /// `decay` was scaled by (see `kb_core::memory::stability_multiplier`).
    /// `None` when `[memory] scoring_v2_stability` is off (**the daemon-wide
    /// default** — unmeasured on the live corpus, pending a bench that loads
    /// the sessions corpus); present (possibly `1.0`, a no-op) when it's on.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub stability: Option<f32>,
    /// MI-W4.1 decomposition — the per-day decay rate `decay`'s exponent
    /// used (`kb_core::memory::decay_k`'s return for this hit's bucket).
    /// Same presence rule as `rank`/`rel`/`decay`/`age_days` (additive,
    /// always `Some` on a live daemon) — lets the `/memory` health-timeline
    /// sparkline project the curve forward without hardcoding the slow/fast
    /// rate constants client-side.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub decay_k: Option<f32>,
    /// MI-W4.2a — per-week injection-count histogram, index 0 = this week
    /// … last index = "`MEMORY_RECALL_WEEKLY_BUCKETS`-1`+` weeks ago",
    /// summed across every kb's `memory_recalls` ledger (invariant #28),
    /// the `/memory` row sparkline's data source. Only populated when the
    /// request set `with_weekly=true`; empty (never absent — `Vec::new()`
    /// serialises as `[]`) otherwise, so a caller can't mistake "didn't
    /// ask" for "asked, and it's genuinely zero everywhere".
    #[serde(default)]
    pub recall_weekly: Vec<u32>,
    /// CT-C1 — `true` when this memory has at least one OPEN `[kb-flag]`
    /// comment (`kb memory flag <id> --reason "…"` — an agent's in-session
    /// "this is wrong" marker, riding kb-comments/1 per invariant #6).
    /// SURFACED, NEVER SCORED: computed strictly AFTER `rerank_with_policy_
    /// scored` already fixed `score`/`rank`/`rel`/`decay` above, over only
    /// the RETURNED page (bounded IO — one `.review/<id>.json` read per
    /// hit, `fetch_review_marks`, which since CT-C4 also yields
    /// `drift_open` from that same single read) — mirrors
    /// `pinned`/`read_pct`'s post-rank enrichment treatment, never
    /// `rerank`'s scoring formula (see the "flagged and unflagged hits at
    /// equal rank tie exactly" unit test).
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub flagged: bool,
    /// CT-C3 — `true` when this memory records a FAILED approach
    /// (`kb remember --failed` — the `outcome:failed` kb-tag paired with
    /// the `kb-outcome: failed` source meta; see
    /// `kb_core::memory::MemoryOutcome`). The recall hook renders such hits
    /// with a "✗ didn't work:" prefix so a negative memory can never read
    /// like a positive fact. SURFACED, NEVER SCORED (CT-C1's `flagged`
    /// posture exactly): derived from the tag already on each candidate's
    /// `DocSummary.tags` (zero extra IO — no lookup at all), collected
    /// during fan-out but APPLIED strictly AFTER `rerank_with_policy_
    /// scored` fixed `score`/`rank`/`rel`/`decay`, onto only the RETURNED
    /// page (`apply_warns`); nothing lands on `kb_core::memory::RecallHit`/
    /// `Scored`, so the scoring formula structurally cannot see it (see
    /// `apply_warns_never_touches_the_scoring_decomposition`).
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub warns: bool,
    /// CT-C4 — the memory's own extracted code-ref PATH hints, from the
    /// kb-LOCAL `code_refs` table (invariant #2: kb extracts HINTS — these
    /// are the doc's own cited paths, never a verdict about any repo; no
    /// kb-code call happens anywhere on this route). Distinct `path_hint`
    /// values of the path-shaped kinds (`path`/`path_line`/`path_range`/
    /// `path_list` — never `issue` org/repo slugs, never gem/vendor
    /// `external`), in document order, capped at [`CODE_HINTS_CAP`] —
    /// `code_hints_total` makes the truncation explicit, never silent.
    /// SURFACED, NEVER SCORED (the CT-C1 posture): one bounded
    /// `code_refs_of` read per RETURNED hit (`fetch_code_hints`), applied
    /// strictly AFTER `rerank_with_policy_scored` fixed
    /// `score`/`rank`/`rel`/`decay` (see the twin isolation unit test).
    /// Omitted when empty so the hook's rendering of ordinary hits stays
    /// byte-identical — the hook deliberately does NOT render this field
    /// at all (json consumers only).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<Vec<String>>", optional))]
    pub code_hints: Vec<String>,
    /// CT-C4 — TOTAL distinct path hints on this memory BEFORE the
    /// [`CODE_HINTS_CAP`] truncation of `code_hints` (`> code_hints.len()`
    /// ⇒ truncated). Omitted when 0 (no path hints at all).
    #[serde(skip_serializing_if = "is_zero_u32", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<u32>", optional))]
    pub code_hints_total: u32,
    /// CT-C4 — count of OPEN `[kb-drift]` comments on this memory
    /// (`/kb-verify`'s "the citation rotted" markers — the ONE grammar,
    /// `kb_core::memory::is_drift_comment`). Read in the SAME single
    /// bounded per-hit `.review/<id>.json` pass as CT-C1's `flagged`
    /// (`fetch_review_marks` — ONE `review::load` per returned hit, never
    /// two), so surfacing drift costs zero extra IO. The hook renders a
    /// " [⚠ N drift-flagged citation(s)]" SUFFIX on such hits — honesty
    /// caveat: the signal is at most one /kb-verify sweep stale (the
    /// red-team killed a live cross-daemon kb-code call inside
    /// UserPromptSubmit as a reliability hazard; sweep-then-flag is the
    /// sanctioned path). SURFACED, NEVER SCORED; omitted when 0.
    #[serde(skip_serializing_if = "is_zero_u32", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<u32>", optional))]
    pub drift_open: u32,
}

/// serde helper for the CT-C4 absent-when-zero counters (the u32 twin of
/// `flagged`/`warns`' `std::ops::Not::not` absent-when-false rule).
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

/// CT-A1 (U3 parse-back) — best-effort parse of the stored `kb_source_anchor`
/// JSON text (`lists::anchor_to_json`'s output) into a structured
/// `review::Anchor`. `None` when absent (not highlight-born) OR when the
/// stored text fails to parse — never a 500; a caller renders the plain
/// artifact link in that case (the HARD FENCE: no re-resolution, no
/// staleness machinery, just an honest absence).
fn parse_source_anchor(raw: Option<&str>) -> Option<kb_core::review::Anchor> {
    serde_json::from_str(raw?).ok()
}

/// MI-W1.3 — write the batched `memory_recalls` aggregate onto each hit,
/// keyed by `(kb, id)`. Pure + IN-PLACE (mutates `hits[i]` by index, never
/// re-sorts/filters/rebuilds the slice), so it structurally cannot reorder
/// or drop a hit — the ordering guarantee `rerank_with_policy` already
/// established survives this pass untouched. Split out from `recall` so it
/// can be unit-tested without a storage actor.
fn apply_recall_stats(
    hits: &mut [RecallResult],
    stats: &HashMap<(String, String), (u32, Option<i64>, u32)>,
) {
    for h in hits.iter_mut() {
        if let Some((count, last, used_count)) = stats.get(&(h.kb.clone(), h.id.clone())) {
            h.recall_count = *count;
            h.last_recalled_at = *last;
            h.recall_used_count = *used_count;
        }
    }
}

/// CT-C1 + CT-C4 — both review-borne marks per hit, from ONE bounded pass.
#[derive(Debug, Default)]
struct ReviewMarks {
    /// `(kb, id)` set of hits carrying at least one OPEN `[kb-flag]`
    /// comment (CT-C1's `flagged`).
    flagged: HashSet<(String, String)>,
    /// `(kb, id)` → count of OPEN `[kb-drift]` comments (CT-C4's
    /// `drift_open`). No zero entries — absent means 0.
    drift_open: HashMap<(String, String), u32>,
}

/// CT-C1 + CT-C4 — bounded per-hit review lookup: exactly ONE
/// `.review/<id>.json` read per hit in `hits` (never a directory walk,
/// never the full corpus — and never TWO reads: the flag bit and the
/// drift count come out of the same loaded file in one comment walk),
/// reusing the same `review::load` path every kb-comments/1 mutation goes
/// through (invariant #6, `crate::routes::comments::with_review_mut`).
/// Called on the already-truncated RETURNED page (`hits.len() <=
/// MAX_LIMIT`), so the worst case is 50 tiny JSON reads. The two prefixes
/// (`[kb-flag] ` / `[kb-drift] `) are disjoint, so one comment body can
/// only ever count toward one mark.
fn fetch_review_marks(paths: &kb_core::paths::KbPaths, hits: &[RecallResult]) -> ReviewMarks {
    let mut out = ReviewMarks::default();
    for h in hits {
        let Ok(kb_name) = KbName::new(&h.kb) else {
            continue;
        };
        let path = paths.kb_review_file(&kb_name, &h.id);
        let Ok(Some(file)) = kb_core::review::load(&path) else {
            continue;
        };
        let mut flagged = false;
        let mut drift: u32 = 0;
        for c in &file.comments {
            if c.status != kb_core::review::CommentStatus::Open {
                continue;
            }
            if kb_core::memory::is_flag_comment(&c.body) {
                flagged = true;
            }
            if kb_core::memory::is_drift_comment(&c.body) {
                drift = drift.saturating_add(1);
            }
        }
        if flagged {
            out.flagged.insert((h.kb.clone(), h.id.clone()));
        }
        if drift > 0 {
            out.drift_open.insert((h.kb.clone(), h.id.clone()), drift);
        }
    }
    out
}

/// CT-C1 — apply the flagged set onto each hit BY `(kb, id)`, in place.
/// Same "never reorders, never drops, never adds" contract as
/// `apply_recall_stats` — and, since it writes ONLY `flagged`, it
/// structurally cannot perturb `score`/`rank`/`rel`/`decay` (SURFACED,
/// NEVER SCORED — see the route's unit test).
fn apply_flagged(hits: &mut [RecallResult], flagged: &HashSet<(String, String)>) {
    for h in hits.iter_mut() {
        h.flagged = flagged.contains(&(h.kb.clone(), h.id.clone()));
    }
}

/// CT-C4 — apply the open-drift counts onto each hit BY `(kb, id)`, in
/// place. Same "never reorders, never drops, never adds" contract as
/// `apply_flagged`/`apply_recall_stats` — and, since it writes ONLY
/// `drift_open`, it structurally cannot perturb `score`/`rank`/`rel`/
/// `decay` (SURFACED, NEVER SCORED — see the twin unit test). Fed from the
/// SAME `fetch_review_marks` pass as `apply_flagged` (one review read per
/// hit serves both fields).
fn apply_drift_open(hits: &mut [RecallResult], drift: &HashMap<(String, String), u32>) {
    for h in hits.iter_mut() {
        h.drift_open = drift
            .get(&(h.kb.clone(), h.id.clone()))
            .copied()
            .unwrap_or(0);
    }
}

/// CT-C3 — apply the failed-outcome set onto each hit BY `(kb, id)`, in
/// place. Same "never reorders, never drops, never adds" contract as
/// `apply_flagged`/`apply_recall_stats` — writing ONLY `warns`, it
/// structurally cannot perturb `score`/`rank`/`rel`/`decay` (SURFACED,
/// NEVER SCORED — see the twin unit test). Unlike CT-C1's `fetch_flagged_
/// ids` there is no lookup to bound: the set was collected for free during
/// fan-out from the `outcome:failed` tag already on each candidate row's
/// `DocSummary.tags` (`tags_csv` rides both `SEARCH_PROJECTION` and the
/// `list_docs` timeline projection), and this pass — the ONLY consumer —
/// runs strictly post-rank.
fn apply_warns(hits: &mut [RecallResult], failed: &HashSet<(String, String)>) {
    for h in hits.iter_mut() {
        h.warns = failed.contains(&(h.kb.clone(), h.id.clone()));
    }
}

/// CT-C4 — is this stored `code_refs` row a FILE-PATH hint? The four
/// path-shaped kinds only (`path`/`path_line`/`path_range`/`path_list`; a
/// `path#member` combined form is kind `path`): an `issue` row's `path_hint`
/// is an org/repo slug, not a file, and an `external` row is a gem/vendor
/// path with no scent value toward THIS repo's code (invariant #2's closed
/// grammar).
///
/// `pub(crate)` because CT-D1's context pack aggregates the SAME rows under a
/// different unit (distinct paths across the pack's artifacts, with a
/// citing-doc count, uncapped) and must not re-spell this filter — the two
/// disagreeing would mean one surface quietly counted `issue` slugs as files.
pub(crate) fn is_path_shaped_kind(kind: &str) -> bool {
    use kb_core::coderefs::CodeRefKind;
    matches!(
        CodeRefKind::parse(kind),
        Some(
            CodeRefKind::Path
                | CodeRefKind::PathLine
                | CodeRefKind::PathRange
                | CodeRefKind::PathList
        )
    )
}

/// CT-C4 — the pure half of the code-hints enrichment: distinct PATH
/// hints from one doc's stored `code_refs` rows, in document (row) order,
/// capped at [`CODE_HINTS_CAP`]. Returns `(capped_paths, total_distinct)`
/// so truncation is always explicit (`total > paths.len()` ⇒ truncated),
/// never silent. Only the path-shaped kinds contribute
/// (`path`/`path_line`/`path_range`/`path_list` — a `path#member` combined
/// form is kind `path`): an `issue` row's `path_hint` is an org/repo slug,
/// not a file, and an `external` row is a gem/vendor path with no scent
/// value toward THIS repo's code (invariant #2's grammar). Dedup is on the
/// `path_hint` itself — `src/a.rs:10` and `src/a.rs:20-30` share one hint
/// (`path_hint` stores the path without its line tail), so the cap counts
/// FILES, not citations.
fn code_hint_paths(refs: &[CodeRefRow]) -> (Vec<String>, u32) {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut paths: Vec<String> = Vec::new();
    let mut total: u32 = 0;
    for r in refs {
        if !is_path_shaped_kind(&r.kind) {
            continue;
        }
        let Some(hint) = r.path_hint.as_deref() else {
            continue;
        };
        if !seen.insert(hint) {
            continue;
        }
        total += 1;
        if paths.len() < CODE_HINTS_CAP {
            paths.push(hint.to_string());
        }
    }
    (paths, total)
}

/// CT-C4 — bounded per-hit code-ref lookup: exactly ONE `code_refs_of`
/// storage read per hit in `hits` (the kb-LOCAL `code_refs` sqlite table —
/// NEVER a kb-code HTTP call: the red-team killed a blocking cross-daemon
/// call inside the UserPromptSubmit hot path as a reliability hazard, and
/// kb-side data is structurally hint-only anyway, invariant #2). Called on
/// the already-truncated RETURNED page (`hits.len() <= MAX_LIMIT`), so the
/// worst case is 50 single-doc sqlite reads through the storage actor. A
/// missing kb, a storage error, or a never-scanned doc all degrade to
/// "no hints" — never a 500, never a guess.
async fn fetch_code_hints(
    state: &KbHandles,
    hits: &[RecallResult],
) -> HashMap<(String, String), (Vec<String>, u32)> {
    let mut out = HashMap::new();
    for h in hits {
        let Ok(kb_name) = KbName::new(&h.kb) else {
            continue;
        };
        let Some(ctx) = state.kbs.get(&kb_name) else {
            continue;
        };
        let doc = match ctx.storage.code_refs_of(h.id.clone()).await {
            Ok(Some(doc)) => doc,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(kb = %h.kb, id = %h.id, error = %e, "recall: code_refs_of failed; hit surfaces without code_hints");
                continue;
            }
        };
        let (paths, total) = code_hint_paths(&doc.refs);
        if total > 0 {
            out.insert((h.kb.clone(), h.id.clone()), (paths, total));
        }
    }
    out
}

/// CT-C4 — apply the code-hint sets onto each hit BY `(kb, id)`, in place.
/// Same "never reorders, never drops, never adds" contract as its sibling
/// passes — writing ONLY `code_hints`/`code_hints_total`, it structurally
/// cannot perturb `score`/`rank`/`rel`/`decay` (SURFACED, NEVER SCORED —
/// see the twin unit test).
fn apply_code_hints(
    hits: &mut [RecallResult],
    hints: &HashMap<(String, String), (Vec<String>, u32)>,
) {
    for h in hits.iter_mut() {
        if let Some((paths, total)) = hints.get(&(h.kb.clone(), h.id.clone())) {
            h.code_hints = paths.clone();
            h.code_hints_total = *total;
        }
    }
}

/// Does this corpus participate in the requested scope? A corpus is a
/// memory corpus only when `memory_scope` is set; `scope=all` spans both
/// kinds, and `scope=project` honours an optional `project=` name filter.
fn in_scope(scope: &str, project: Option<&str>, name: &KbName, ctx: &KbContext) -> bool {
    match ctx.memory_scope.as_deref() {
        Some("global") => scope == "all" || scope == "global",
        Some("project") => {
            let scope_ok = scope == "all" || scope == "project";
            let name_ok = match project {
                Some(p) => name.as_str() == p,
                None => true,
            };
            scope_ok && name_ok
        }
        _ => false,
    }
}

pub async fn recall(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<Params>,
) -> Response<Body> {
    match recall_compose(state, identity, params).await {
        Ok(out) => Json(out).into_response(),
        Err(resp) => resp,
    }
}

/// The recall engine, split out of the axum handler so a SECOND in-process
/// caller can compose it without an HTTP round-trip: CT-D1's
/// `GET /api/context` pack (`routes::context`) is exactly the recall lane it
/// already returns, budgeted and joined to three sibling reads. The handler
/// above is now a thin `Ok → Json` / `Err → problem+json` wrapper, so the
/// wire shape of `/api/memory/recall` is byte-identical to pre-CT-D1 (the
/// `Err` arm carries the very `error_to_problem_json` response the old
/// inline `return` produced).
///
/// Takes its arguments OWNED, exactly as the extractors delivered them, so
/// the (large) body below is unchanged from the handler it was lifted out
/// of — a borrowed signature would have rippled through every `&state`
/// capture in the fan-out closures for no benefit.
#[allow(clippy::result_large_err)]
pub(crate) async fn recall_compose(
    state: Arc<KbHandles>,
    identity: crate::middleware::Identity,
    params: Params,
) -> Result<RecallResponse, Response<Body>> {
    if !matches!(params.scope.as_str(), "all" | "global" | "project") {
        return Err(error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "unsupported scope {:?}; expected one of: all, global, project",
            params.scope
        ))));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let user = identity.user.clone();
    // Over-fetch per corpus so a high-salience hit ranked low in a busy
    // corpus survives the global merge.
    let per_corpus = ((limit * 4).clamp(20, MAX_LIMIT)) as u32;
    let started = Instant::now();

    // In-scope corpora in deterministic BTreeMap order. Cache each one's
    // source_path for the source_relative on the response.
    let corpora: Vec<(&KbName, &KbContext)> = state
        .kbs
        .iter()
        .filter(|(name, ctx)| in_scope(&params.scope, params.project.as_deref(), name, ctx))
        .collect();
    let source_paths: HashMap<String, PathBuf> = corpora
        .iter()
        .map(|(name, ctx)| (name.to_string(), ctx.source_path.clone()))
        .collect();

    let has_query = !params.q.trim().is_empty();

    // Embed the query once per distinct embedder MODEL, reusing the
    // result across same-model corpora. The daemon's LRU
    // (`state.embed_cache`) also serves repeats across requests, and the
    // embed call runs inside `spawn_blocking` so it doesn't pin a tokio
    // worker. Keying on model name (not dim) avoids a latent
    // cross-model bug: two distinct models can share a dim (e.g.
    // jina-v2-base-code and bge-base-en-v1.5 are both 768), and feeding
    // one model's vector to another model's index returns garbage.
    let mut vec_by_model: HashMap<&'static str, Vec<f32>> = HashMap::new();
    if has_query {
        for (_, ctx) in &corpora {
            if let Some(emb) = &ctx.embedder {
                let model = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
                if let std::collections::hash_map::Entry::Vacant(slot) = vec_by_model.entry(model) {
                    if let Ok(out) =
                        crate::embed_cache::embed_query(&state.embed_cache, emb, &params.q).await
                    {
                        slot.insert(out.vec);
                    }
                }
            }
        }
    }

    let mut all_hits: Vec<RecallHit> = Vec::new();
    let mut tombstones: HashSet<String> = HashSet::new();
    // M2 — pinned set per kb. Pinned memories survive the decay floor.
    let mut pinned_by_kb: HashMap<String, HashSet<String>> = HashMap::new();
    // X1 — kbs whose pinned-set read FAILED. We can't tell pinned from
    // unpinned for these, so the decay floor is skipped for them (better
    // to surface a low-salience memory than to silently drop a genuinely-
    // pinned one on a transient sqlite error).
    let mut pinned_unknown: HashSet<String> = HashSet::new();
    // L7 — per-id link set, accumulated across corpora. Keyed on the
    // hit's path-based artifact id (unique across the daemon). Used
    // both for the inline `for_kb` post-filter inside the per-corpus
    // loop AND for projecting link info onto the final response.
    let mut links_by_id: HashMap<String, HashSet<String>> = HashMap::new();
    // L7 normalise: empty string == None (the SPA passes empty when
    // there's no kb context).
    let for_kb = params
        .for_kb
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // CT-B2 — `visible_to` csv → a HashSet, parsed ONCE up front (never
    // per-hit). Deliberately NOT unified with `for_kb`'s normalization
    // above: `for_kb` treats an absent/empty string as "no filter";
    // `visible_to` treats an absent PARAM as "no filter" but a present,
    // syntactically-empty csv as an (unreachable in practice — callers
    // only ever send a non-empty csv, see kb-cli's `resolve_recall_wire`)
    // empty allowlist. The `Option` itself carries the absent/present
    // distinction; only the per-entry tokens are trimmed and emptied
    // tokens dropped.
    let visible_to: Option<HashSet<String>> = params.visible_to.as_deref().map(|csv| {
        csv.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    });
    // v0.13 — per-kb decay-policy override; falls back to the daemon
    // cell. Applied INLINE during fan-out (not later in rerank) so each
    // hit gets the right per-corpus threshold. rerank_with_policy is
    // then called with DecayPolicy::Loose to skip its own floor (the
    // per-hit drops already happened).
    let daemon_policy = *state.memory_policy.read().await;
    // FF-C — gather each corpus's pinned set, links, rows, tombstones and
    // policy concurrently (bounded, submission-ordered), `join!`-ing the
    // independent per-corpus reads. The decay-floor + for_kb filter runs AFTER
    // the fold so it sees the complete links_by_id / pinned_by_kb — its result
    // is unchanged (a memory's links live only in its owning corpus) and the
    // globals stay authoritative for the response projection. No
    // std::sync::Mutex guard crosses an await (invariant 15): model_name()
    // returns &'static str, so the embedder guard drops at the `let model`.
    struct RecallArm {
        pinned: Option<HashSet<String>>,
        links: HashMap<String, HashSet<String>>,
        rows: Vec<kb_core::storage::lance::DocSummary>,
        tombstones: Vec<String>,
        policy: DecayPolicy,
    }
    type RecallArmFut<'a> =
        std::pin::Pin<Box<dyn std::future::Future<Output = (&'a KbName, RecallArm)> + Send + 'a>>;
    let vec_by_model = &vec_by_model;
    let params = &params;
    let mut futs: Vec<RecallArmFut<'_>> = Vec::new();
    for &(name, ctx) in &corpora {
        futs.push(Box::pin(async move {
            let (pinned, links, fts, supersede) = tokio::join!(
                ctx.storage.pinned_memories_set(),
                ctx.storage.memory_links_all(),
                ctx.storage.ensure_fts_index(),
                ctx.storage.list_supersede_targets(),
            );
            let pinned = match pinned {
                Ok(set) => Some(set),
                Err(e) => {
                    tracing::warn!(
                        kb = %name,
                        error = %e,
                        "recall: pinned-set read failed; skipping the decay floor for this corpus so a pinned memory isn't dropped"
                    );
                    None
                }
            };
            // ensure_fts_index is Ok when the index already exists; a real
            // Err would make BM25 fail with a less-specific message, so
            // surface it here and fall back to empty rows for this corpus
            // (invariant #28 — one corpus never 500s the fleet).
            if let Err(e) = fts {
                if has_query {
                    tracing::warn!(
                        kb = %name,
                        error = %e,
                        "recall: ensure_fts_index failed; skipping corpus query arm"
                    );
                    return (
                        name,
                        RecallArm {
                            pinned,
                            links: links.unwrap_or_default(),
                            rows: Vec::new(),
                            tombstones: supersede.unwrap_or_default(),
                            policy: ctx.memory_decay_policy.unwrap_or(daemon_policy),
                        },
                    );
                }
                // Empty-query path uses list_docs (no FTS); keep going.
                tracing::warn!(
                    kb = %name,
                    error = %e,
                    "recall: ensure_fts_index failed; continuing list_docs path"
                );
            }
            let rows = if has_query {
                match &ctx.embedder {
                    Some(emb) => {
                        let model = emb.lock().unwrap_or_else(|e| e.into_inner()).model_name();
                        if let Err(e) = ctx.storage.ensure_vector_index().await {
                            tracing::warn!(
                                kb = %name,
                                error = %e,
                                "recall: ensure_vector_index failed; falling back to BM25"
                            );
                            ctx.storage
                                .bm25_query(params.q.clone(), per_corpus, false)
                                .await
                        } else {
                            match vec_by_model.get(model) {
                                Some(qv) => {
                                    ctx.storage
                                        .hybrid_query(params.q.clone(), qv.clone(), per_corpus)
                                        .await
                                }
                                // Embed failed for this model → keyword fallback.
                                None => {
                                    ctx.storage
                                        .bm25_query(params.q.clone(), per_corpus, false)
                                        .await
                                }
                            }
                        }
                    }
                    // No embedder configured → keyword fallback.
                    None => ctx.storage.bm25_query(params.q.clone(), per_corpus, false).await,
                }
            } else {
                // Loose/empty query → recency timeline.
                ctx.storage.list_docs(per_corpus).await
            };
            (
                name,
                RecallArm {
                    pinned,
                    links: links.unwrap_or_default(),
                    rows: rows.unwrap_or_default(),
                    tombstones: supersede.unwrap_or_default(),
                    policy: ctx.memory_decay_policy.unwrap_or(daemon_policy),
                },
            )
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let arms = super::buffered_join(futs, state.fanout_cap).await;

    // Fold in submission (BTreeMap) order: build the authoritative globals and
    // stash each corpus's rows + policy for the filter pass.
    let mut per_corpus_rows: Vec<(
        String,
        Vec<kb_core::storage::lance::DocSummary>,
        DecayPolicy,
    )> = Vec::new();
    for (name, arm) in arms {
        let kb_str = name.as_str().to_string();
        match arm.pinned {
            Some(set) => {
                pinned_by_kb.insert(kb_str.clone(), set);
            }
            None => {
                pinned_unknown.insert(kb_str.clone());
            }
        }
        for (id, kbs) in arm.links {
            links_by_id.entry(id).or_default().extend(kbs);
        }
        tombstones.extend(arm.tombstones);
        per_corpus_rows.push((kb_str, arm.rows, arm.policy));
    }

    // CT-C3 — `(kb, id)` set of candidate rows carrying the failed-outcome
    // tag, read off `DocSummary.tags` during the filter pass below (free —
    // `tags_csv` is already projected on both row sources). NEVER an input
    // to ranking: `RecallHit` has no tags/warns field, and the set's only
    // consumer is the post-rank `apply_warns` pass further down.
    let mut failed_outcome_ids: HashSet<(String, String)> = HashSet::new();
    // Filter pass — per-corpus decay floor + for_kb visibility, in submission
    // order, with the globals now complete. Byte-identical to the serial inline
    // version (rank is per-corpus; pinned/for_kb read only this corpus's data).
    for (kb_str, rows, effective_policy) in per_corpus_rows {
        // RA-recall — `no_floor` drops the salience floor entirely for this
        // call (used by the dedup oracle). `rerank` is still called with
        // Loose below, so nothing double-drops; the ranking math is unchanged.
        let floor = if params.no_floor {
            f32::NEG_INFINITY
        } else {
            effective_policy.drop_threshold()
        };
        for (rank, ds) in rows.into_iter().enumerate() {
            // R0 — never push a raw session transcript into the every-turn
            // recall. The [kb.sessions] corpus participates in recall
            // (memory_scope=global) but its rows are episodic logs, not curated
            // facts; surface them via /api/sessions or `kb recollect` instead.
            // kb_category is projected by both the query path (SEARCH_PROJECTION)
            // and the empty-query timeline path (list_docs). Keep the original
            // `rank` for the surviving rows — a skipped session leaves a
            // harmless gap (rel = 1/(60+rank) is insensitive to it).
            if !kb_core::memory::is_recallable_memory_category(ds.kb_category.as_deref()) {
                continue;
            }
            let pinned = pinned_by_kb
                .get(&kb_str)
                .map(|s| s.contains(&ds.id))
                .unwrap_or(false);
            // v0.13 — apply the per-kb policy floor inline. Pinned
            // always survives (matches rerank_with_policy semantics).
            // A memory without a kb-salience meta defaults to
            // kb_core::memory::DEFAULT_SALIENCE (re-exported so this floor
            // and rerank's own floor can never drift).
            // Skip the floor entirely for a corpus whose pinned set we
            // couldn't read — we might otherwise drop a pinned memory.
            let floor_applies = !pinned_unknown.contains(&kb_str);
            if floor_applies && !pinned && ds.kb_salience.unwrap_or(DEFAULT_SALIENCE) <= floor {
                continue;
            }
            // L7 — per-id visibility filter. When `for_kb` is set, drop hits
            // whose link set contains neither `*` nor the target kb. Memories
            // with NO link rows at all (race: indexed after L4 backfill ran,
            // before L3 seeded them) are treated as invisible — the seeded
            // tombstone guarantees this window is one-write wide.
            if let Some(target) = for_kb {
                let visible = links_by_id
                    .get(&ds.id)
                    .map(|set| set.contains("*") || set.contains(target))
                    .unwrap_or(false);
                if !visible {
                    continue;
                }
            }
            // CT-B2 — `visible_to`: the INVERSE-default sibling of the
            // `for_kb` filter above, at the same loop position (so the
            // post-filter survivor set feeding scoring_v2_relevance
            // normalization, invariant #10, is consistent between the
            // two). A hit with NO link rows at all is visible everywhere
            // (unlinked memories are the shared commons); a hit with a
            // non-empty link set is visible only when it carries `*` or
            // intersects `visible_to`. Composes with `for_kb` — a hit
            // must survive BOTH filters (AND), never OR'd together.
            if let Some(allowed) = &visible_to {
                let visible = links_by_id
                    .get(&ds.id)
                    .map(|set| set.is_empty() || set.contains("*") || !set.is_disjoint(allowed))
                    .unwrap_or(true);
                if !visible {
                    continue;
                }
            }
            // CT-C3 — record the failed-outcome marker BEFORE `ds.id` moves
            // into the scoring hit below; the hit itself never carries it.
            if kb_core::memory::has_failed_outcome_tag(&ds.tags) {
                failed_outcome_ids.insert((kb_str.clone(), ds.id.clone()));
            }
            all_hits.push(RecallHit {
                kb: kb_str.clone(),
                id: ds.id,
                title: ds.title,
                path: ds.path,
                rank,
                salience: ds.kb_salience,
                decay: ds.kb_decay,
                // RA3 — decay basis = write-time `kb-created` (stable across
                // reindex), falling back to file mtime for pre-RA3 memories.
                mtime_unix: ds.created_unix.or(ds.mtime_unix),
                status: ds.kb_status,
                pinned,
                session_id: ds.kb_session,
                summary: ds.kb_summary,
                memory_type: ds.kb_memory_type,
                source: ds.kb_source,
                author: ds.kb_author,
                source_kb: ds.kb_source_kb,
                source_artifact: ds.kb_source_artifact,
                source_anchor: ds.kb_source_anchor,
                // MI-W2.1 — the raw search-engine score, when this corpus's
                // query arm produced one (`None` on the list_docs timeline
                // path). MI-W2.2's fields are filled in just below, ONLY
                // when scoring_v2 is on (an extra fan-out the default-off
                // path skips entirely).
                score: ds.score,
                recall_count: 0,
                last_recalled_at: None,
            });
        }
    }

    // MI-W2.2, split MI-W5.R — pre-rerank recall-usage fetch, gated behind
    // scoring_v2_stability specifically (relevance never reads this data):
    // the stability term needs recall_count/last_recalled_at as SCORING
    // inputs (unlike MI-W1.3's post-rerank enrichment below, which is
    // display-only and always runs). Only paid for when the flag is on.
    let memory_cfg = state.config.read().await.memory.clone();
    let scoring_v2_relevance = memory_cfg.scoring_v2_relevance;
    let scoring_v2_stability = memory_cfg.scoring_v2_stability;
    if scoring_v2_stability {
        let mut ids_by_kb: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for h in &all_hits {
            ids_by_kb
                .entry(h.kb.clone())
                .or_default()
                .push(h.id.clone());
        }
        let stats = fetch_recall_stats(&state, &ids_by_kb).await;
        // CT-C5 — `used_count` (the tuple's 3rd slot) is deliberately
        // discarded here: `kb_core::memory::RecallHit` (the SCORING struct,
        // distinct from this route's own wire `RecallResult`) has no such
        // field, and the stability term below only ever reads
        // `recall_count`/`last_recalled_at` — surfaced-never-scored.
        for h in &mut all_hits {
            if let Some((count, last, _used_count)) = stats.get(&(h.kb.clone(), h.id.clone())) {
                h.recall_count = *count;
                h.last_recalled_at = *last;
            }
        }
    }

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // v0.13 — per-kb floor was applied during fan-out; pass Loose here
    // so rerank doesn't double-drop. MI-W2.1/2.2, split MI-W5.R —
    // `scoring_v2_relevance`/`scoring_v2_stability` independently gate the
    // relevance-normalization + stability factors; both off reproduces the
    // pre-W2 formula bit-for-bit.
    let scored = rerank_with_policy_scored(
        all_hits,
        &tombstones,
        now_unix,
        limit,
        DecayPolicy::Loose,
        scoring_v2_relevance,
        scoring_v2_stability,
    );
    let ms = started.elapsed().as_millis() as u64;

    let mut hits: Vec<RecallResult> = scored
        .into_iter()
        .map(|s| {
            let source_relative = source_paths
                .get(&s.kb)
                .map(|sp| kb_core::paths::doc_rel_path(&s.path, sp))
                .unwrap_or_else(|| s.path.clone());
            let pinned = pinned_by_kb
                .get(&s.kb)
                .map(|set| set.contains(&s.id))
                .unwrap_or(false);
            // L7 — project the link set onto the response. Split out
            // the `*` sentinel into the `global` flag and sort the
            // remaining names for stable rendering. `unwrap_or_default`
            // covers memories with no link row at all (returns
            // global=false, linked_kbs=[]).
            let raw_links = links_by_id.get(&s.id);
            let global = raw_links.map(|set| set.contains("*")).unwrap_or(false);
            let mut linked_kbs: Vec<String> = raw_links
                .map(|set| set.iter().filter(|k| k.as_str() != "*").cloned().collect())
                .unwrap_or_default();
            linked_kbs.sort();
            RecallResult {
                id: s.id,
                kb: s.kb,
                title: s.title,
                path: s.path,
                source_relative,
                score: s.score,
                salience: s.salience,
                pinned,
                session_id: s.session_id,
                summary: s.summary,
                memory_type: s.memory_type,
                source: s.source,
                author: s.author,
                source_kb: s.source_kb,
                source_artifact: s.source_artifact,
                source_anchor: parse_source_anchor(s.source_anchor.as_deref()),
                global,
                linked_kbs,
                read_pct: None,
                last_read_at: None,
                stopped_at: None,
                rank: Some(s.rank),
                rel: Some(s.rel),
                decay: Some(s.decay),
                age_days: Some(s.age_days),
                recall_count: 0,
                last_recalled_at: None,
                recall_used_count: 0,
                relevance_factor: s.relevance_factor,
                stability: s.stability,
                decay_k: Some(s.decay_k),
                recall_weekly: Vec::new(),
                // CT-C1 — placeholder; `apply_flagged` fills this in
                // (post-rank, bounded to the returned page) just below.
                flagged: false,
                // CT-C3 — placeholder; `apply_warns` fills this in from the
                // fan-out-collected `failed_outcome_ids` just below.
                warns: false,
                // CT-C4 — placeholders; `apply_code_hints` /
                // `apply_drift_open` fill these in (post-rank, bounded to
                // the returned page) just below.
                code_hints: Vec::new(),
                code_hints_total: 0,
                drift_open: 0,
            }
        })
        .collect();

    // CT-C1 + CT-C4 — review-borne marks (flagged + open-drift count),
    // bounded to the RETURNED page from ONE `.review/<id>.json` read per
    // hit (see `fetch_review_marks`'s doc comment). Runs before the
    // read_pct/recall_count passes below only because it's cheapest to
    // reason about first; ordering among these post-rank enrichment passes
    // doesn't matter — none of them touch each other's fields.
    let marks = fetch_review_marks(&state.paths, &hits);
    apply_flagged(&mut hits, &marks.flagged);
    apply_drift_open(&mut hits, &marks.drift_open);
    // CT-C3 — failed-outcome warning, same post-rank slot (field-disjoint
    // from every other enrichment pass; see `apply_warns`'s doc comment).
    apply_warns(&mut hits, &failed_outcome_ids);
    // CT-C4 — kb-local code-ref path hints, same post-rank slot (one
    // bounded `code_refs_of` read per returned hit; see
    // `fetch_code_hints`'s doc comment — never a kb-code call).
    let code_hints = fetch_code_hints(&state, &hits).await;
    apply_code_hints(&mut hits, &code_hints);

    // RP-track — best-effort recall enrichment: latest-visit reading state
    // per hit. Group the hits by kb and issue ONE batched
    // `reading_latest_for_ids` per kb (a single window pass) instead of one
    // serial actor round-trip per hit. Deliberately NOT cached — read-state
    // changes on every scroll, and scroll UPDATEs emit no SSE to invalidate a
    // cache, so a TouchesCache-style LRU would serve stale read%. A failure /
    // missing kb / capture-off just leaves the fields absent. (Runs after the
    // `ms` capture: it measures search latency, not this add-on.)
    let mut idx_by_kb: std::collections::BTreeMap<KbName, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (i, h) in hits.iter().enumerate() {
        if let Ok(kb_name) = KbName::new(&h.kb) {
            idx_by_kb.entry(kb_name).or_default().push(i);
        }
    }
    for (kb_name, idxs) in idx_by_kb {
        let Some(ctx) = state.kbs.get(&kb_name) else {
            continue;
        };
        if !ctx.reading_progress {
            continue;
        }
        let ids: Vec<String> = idxs.iter().map(|&i| hits[i].id.clone()).collect();
        let latest = ctx
            .storage
            .reading_latest_for_ids(ids, user.clone())
            .await
            .unwrap_or_default();
        for i in idxs {
            if let Some((pct, last_section, last_at)) = latest.get(&hits[i].id) {
                hits[i].read_pct = Some(*pct);
                hits[i].last_read_at = Some(*last_at);
                hits[i].stopped_at = last_section.clone();
            }
        }
    }

    // MI-W1.3 — recall-usage enrichment: recall_count + last_recalled_at
    // per hit. Display enrichment ONLY, computed strictly after rerank —
    // `apply_recall_stats` never touches ordering. Unlike read_pct's
    // per-(hit-kb) batching, the ledger lives with the RECALLING session's
    // kb, not the memory's own kb — so for each distinct hit-kb we still
    // fan out across EVERY kb in the daemon (invariant #28), bounded by one
    // shared `buffered_join` call for the whole batch (≤50 hits × however
    // many distinct kbs they span × every kb — small in practice: a recall
    // response rarely spans more than a handful of corpora). Reuses the
    // SAME `fetch_recall_stats` helper the MI-W2.2 pre-rerank fetch above
    // calls — this pass is unconditional (runs whether or not `scoring_v2`
    // is on) because it's display, not scoring.
    let mut ids_by_hit_kb: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for h in &hits {
        ids_by_hit_kb
            .entry(h.kb.clone())
            .or_default()
            .push(h.id.clone());
    }
    let recall_stats = fetch_recall_stats(&state, &ids_by_hit_kb).await;
    apply_recall_stats(&mut hits, &recall_stats);

    // MI-W4.2a — opt-in per-week histogram, same fan-out shape as the
    // recall-stats pass just above but gated on `with_weekly` (the hot
    // per-turn `kb-recall` hook never sets it, so this costs it nothing).
    if params.with_weekly {
        let weekly = fetch_recall_weekly(&state, &ids_by_hit_kb, now_unix).await;
        for h in &mut hits {
            if let Some(w) = weekly.get(&(h.kb.clone(), h.id.clone())) {
                h.recall_weekly = w.clone();
            }
        }
    }

    Ok(RecallResponse { hits, ms })
}

/// Shared recall-usage fan-out: for every `(hit_kb, ids)` pair, sum
/// `count`/`used_count` and max `last_recalled_at` across EVERY kb's
/// `memory_recalls` ledger that names `hit_kb` as the memory's own corpus
/// (invariant #28 — the ledger lives with the RECALLING session's kb, not
/// the memory's). Shared by MI-W2.2's pre-rerank scoring fetch (gated on
/// `scoring_v2_stability`, which reads ONLY `count`/`last_recalled_at` —
/// `used_count` rides along in the tuple but that call site discards it,
/// never touching the scorer, CT-C5) and MI-W1.3's unconditional post-rerank
/// display enrichment (which surfaces all three).
async fn fetch_recall_stats(
    state: &KbHandles,
    ids_by_kb: &std::collections::BTreeMap<String, Vec<String>>,
) -> HashMap<(String, String), (u32, Option<i64>, u32)> {
    let mut futs: Vec<
        super::CorpusFut<'_, (String, Vec<kb_core::storage::sqlite::MemoryRecallCount>)>,
    > = Vec::new();
    for (hit_kb, ids) in ids_by_kb {
        for (kb2, ctx2) in state.kbs.iter() {
            let ids = ids.clone();
            let hit_kb = hit_kb.clone();
            futs.push(Box::pin(async move {
                let parts = ctx2
                    .storage
                    .memory_recalls_counts_for_ids(Some(hit_kb.clone()), ids)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(kb = %kb2, error = %e, "memory_recalls_counts_for_ids failed");
                        Vec::new()
                    });
                (hit_kb, parts)
            }));
        }
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let mut recall_stats: HashMap<(String, String), (u32, Option<i64>, u32)> = HashMap::new();
    for (hit_kb, parts) in partials {
        for c in parts {
            let e = recall_stats
                .entry((hit_kb.clone(), c.memory_id))
                .or_insert((0, None, 0));
            e.0 = e.0.saturating_add(c.count);
            e.1 = match (e.1, c.last_recalled_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            e.2 = e.2.saturating_add(c.used_count);
        }
    }
    recall_stats
}

/// MI-W4.2a — shared per-week histogram fan-out, sibling of
/// `fetch_recall_stats` (same `(hit_kb, ids)` shape, same invariant #28
/// fan-out, same "the ledger lives with the RECALLING session's kb"
/// reasoning) but summing INTO a fixed-width bucket vector instead of a
/// single count. Callers gate this behind an opt-in (`recall`'s
/// `with_weekly` param) — unlike `fetch_recall_stats`, this is NOT called
/// unconditionally, since the hot per-turn `kb-recall` hook path has no use
/// for a histogram and shouldn't pay for one on every prompt.
async fn fetch_recall_weekly(
    state: &KbHandles,
    ids_by_kb: &std::collections::BTreeMap<String, Vec<String>>,
    now_unix: i64,
) -> HashMap<(String, String), Vec<u32>> {
    let buckets = kb_core::storage::sqlite::MEMORY_RECALL_WEEKLY_BUCKETS as usize;
    let mut futs: Vec<
        super::CorpusFut<'_, (String, Vec<kb_core::storage::sqlite::MemoryRecallWeeklyRow>)>,
    > = Vec::new();
    for (hit_kb, ids) in ids_by_kb {
        for (kb2, ctx2) in state.kbs.iter() {
            let ids = ids.clone();
            let hit_kb = hit_kb.clone();
            futs.push(Box::pin(async move {
                let parts = ctx2
                    .storage
                    .memory_recalls_weekly_for_ids(Some(hit_kb.clone()), ids, now_unix)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(kb = %kb2, error = %e, "memory_recalls_weekly_for_ids failed");
                        Vec::new()
                    });
                (hit_kb, parts)
            }));
        }
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let mut weekly: HashMap<(String, String), Vec<u32>> = HashMap::new();
    for (hit_kb, parts) in partials {
        for row in parts {
            let entry = weekly
                .entry((hit_kb.clone(), row.memory_id))
                .or_insert_with(|| vec![0u32; buckets]);
            let idx = (row.weeks_ago.max(0) as usize).min(buckets.saturating_sub(1));
            entry[idx] = entry[idx].saturating_add(row.count);
        }
    }
    weekly
}

// === MI-W1.2 — memory census ===============================================

const CENSUS_DEFAULT_LIMIT: u32 = 50;
const CENSUS_MAX_LIMIT: u32 = 200;

/// Per-memory-id recall-ledger aggregate: `(recall_count,
/// last_recalled_at, recall_used_count)` — the tuple `census_recall_stats`
/// folds per-kb `MemoryRecallCount` partials into.
type CensusRecallStats = HashMap<String, (u32, Option<i64>, u32)>;

#[derive(Debug, Deserialize)]
pub struct CensusParams {
    /// The ONE memory corpus to scan. A caller wanting every corpus's
    /// census iterates this itself — no cross-corpus merge math here
    /// (mirrors `/memory/recall`'s per-corpus `for_kb`, not its fan-out).
    pub kb: String,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    /// MI-W3.3a — optional facet filter: exact match against
    /// `kb-memory-type` (`episodic` | `semantic` | `procedural`). Absent ⇒
    /// no filter (includes untyped memories). Applied BEFORE pagination
    /// (`offset`/`limit` and `total` all reflect the filtered set), same
    /// order as the existing memory-category filter.
    #[serde(rename = "type")]
    pub memory_type: Option<String>,
    /// CT-E4 — optional named ordering. The ONLY term is `unverified`:
    /// agent-hot-human-cold rows first — the (recall_count > 0 AND never
    /// opened by this request's user) bucket, recall_count DESC within/
    /// after it, id ASC (the default order) as the tie-break. Absent/empty
    /// keeps the plain id-ASC paging order byte-identical; any other value
    /// is a 400, never a silent ignore.
    pub sort: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryCensusRow")
)]
#[derive(Debug, Serialize)]
pub struct CensusRow {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub category: Option<String>,
    /// `None` when the artifact has no `kb-salience` meta (absent, not 0).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub salience: Option<f32>,
    /// The raw `kb-decay` bucket ("slow" | "fast"), not a computed float —
    /// same categorical value `memory::rerank` reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub decay_bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub created_unix: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub age_days: Option<f32>,
    pub pinned: bool,
    pub global: bool,
    pub linked_kbs: Vec<String>,
    /// MI-W2.3 — `true` when `kb-status == "forgotten"` (soft-forgotten via
    /// `DELETE …/artifacts/{id}` without `?purge`). This is the whole point
    /// of a tombstone over a hard delete: census still LISTS a forgotten
    /// memory (unlike `recall`, which drops it) so an operator can see and
    /// audit what's been forgotten, not just what still ranks.
    #[serde(default)]
    pub forgotten: bool,
    /// CT-C3 — `true` when this memory records a FAILED approach
    /// (`kb remember --failed`): the `outcome:failed` tag (the indexed
    /// carrier, also visible verbatim in `tags` below) paired with the
    /// `kb-outcome: failed` source meta. Same absent-when-false wire shape
    /// as recall's `warns`; display only, never a scoring input.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub supersedes: Option<String>,
    /// Reverse of `supersedes`, resolved WITHIN this corpus only (the
    /// corpus-local reads that back it — `list_docs` — are per-kb).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub superseded_by: Option<String>,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_id: Option<String>,
    /// MI-W1.1 — how many times a `kb-recall` hook actually injected this
    /// memory into a captured session, summed across every kb's
    /// `memory_recalls` ledger (the ledger lives with the RECALLING
    /// session, not this memory's own corpus).
    pub recall_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub last_recalled_at: Option<i64>,
    /// CT-C5 (V0037) — how many of `recall_count`'s injections the
    /// recalling session went on to EXPLICITLY REFERENCE (the memory's id
    /// or title, verbatim, in a later turn) — always `<= recall_count`. This
    /// is a lower bound on usefulness, not a full one: an agent can act on a
    /// recalled fact without ever naming it, and that case is
    /// indistinguishable from "unreferenced" here. Display only — never a
    /// scoring input.
    pub recall_used_count: u32,
    /// MI-W3.3a — optional CoALA-minimal type from `<meta
    /// name="kb-memory-type">`. Absent (untyped) for the vast majority of
    /// the corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub memory_type: Option<String>,
    /// MI-W3.4 — write-time trust tag from `<meta name="kb-source">`.
    /// SURFACED (display only), NEVER a scoring input.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source: Option<String>,
    /// CT-A1 (U3 parse-back) — the `you`/`claude` role this memory was
    /// highlighted under. Absent for the vast majority of the corpus.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_kb: Option<String>,
    /// CT-A1 — artifact id of that origin artifact.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_artifact: Option<String>,
    /// CT-A1 — the origin selection, best-effort parsed into a
    /// `review::Anchor` (see `parse_source_anchor`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_anchor: Option<kb_core::review::Anchor>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryCensusResponse")
)]
#[derive(Debug, Serialize)]
pub struct CensusResponse {
    pub rows: Vec<CensusRow>,
    /// The TRUE total count of memory artifacts in this corpus (uncapped —
    /// not clamped to `limit`), so a caller can page to the end.
    pub total: u32,
    pub offset: u32,
    pub limit: u32,
}

/// `GET /api/memory/census?kb=<memory-corpus>[&offset=][&limit=][&sort=unverified]`
/// — a plain, uncapped-total paginated scan of ONE memory corpus's artifacts (id,
/// salience, decay bucket, age, pin/link/supersede state, tags, origin
/// session) plus recall-usage stats. `recall_count`/`last_recalled_at`/
/// `recall_used_count` (CT-C5, V0037 — how many of those injections were
/// EXPLICITLY REFERENCED later) are aggregated by fanning out across every
/// kb's `memory_recalls` ledger (invariant #28, `buffered_join`) and summing
/// the per-kb partials — most kbs contribute nothing (only a kb that
/// captures `memory-session` transcripts populates its ledger).
pub async fn census(
    State(state): State<Arc<KbHandles>>,
    Extension(identity): Extension<crate::middleware::Identity>,
    Query(params): Query<CensusParams>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &params.kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // CT-E4 — validate ?sort= up front (before any storage IO): absent/
    // empty ⇒ the default id-ASC order, `unverified` ⇒ agent-hot-human-cold
    // first, anything else ⇒ 400.
    let sort_unverified = match params.sort.as_deref().map(str::trim) {
        None | Some("") => false,
        Some("unverified") => true,
        Some(other) => {
            return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                "unsupported sort {other:?}; expected \"unverified\""
            )))
        }
    };
    let offset = params.offset.unwrap_or(0);
    let limit = params
        .limit
        .unwrap_or(CENSUS_DEFAULT_LIMIT)
        .min(CENSUS_MAX_LIMIT);

    let docs = match ctx.storage.list_docs(u32::MAX).await {
        Ok(d) => d,
        Err(e) => return error_to_problem_json(&e),
    };
    // Memory artifacts only, excluding `memory-session` transcripts — R0
    // already excludes them from recall/search (invariant #11), and a
    // transcript would otherwise show up as an un-recallable "memory" with
    // zero salience/pin/link/recall data. MI-W4.0 — this MUST be the same
    // predicate `recall` applies (`is_recallable_memory_category`), not an
    // independent `starts_with("memory-")` guess: a memory-scoped corpus's
    // docs recall serves are not all tagged with a `"memory-"`-prefixed
    // category (e.g. a live "project"-categoried row), and a census that
    // hides them under-reports the very population it exists to report.
    let mut memories: Vec<_> = docs
        .into_iter()
        .filter(|d| kb_core::memory::is_recallable_memory_category(d.kb_category.as_deref()))
        .collect();
    // Deterministic paging order — `list_docs` makes no ordering promise.
    memories.sort_by(|a, b| a.id.cmp(&b.id));

    // Reverse `kb_supersedes` map, built over the FULL corpus (before
    // pagination) so a superseder outside the current page still resolves.
    let mut superseded_by: HashMap<String, String> = HashMap::new();
    for d in &memories {
        if let Some(target) = &d.kb_supersedes {
            superseded_by
                .entry(target.clone())
                .or_insert_with(|| d.id.clone());
        }
    }

    // MI-W3.3a — optional facet filter, applied AFTER the superseded_by
    // reverse-map build (that map must reflect corpus-wide truth regardless
    // of which rows this call chooses to return) but BEFORE `total`/paging,
    // so both honour the filter.
    if let Some(want) = params
        .memory_type
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        memories.retain(|d| d.kb_memory_type.as_deref() == Some(want));
    }

    let total = memories.len() as u32;

    // CT-E4 — ?sort=unverified needs every FILTERED row's recall count +
    // read state BEFORE pagination (the whole point is pulling agent-hot-
    // human-cold rows onto page one), so this branch runs the same two
    // EXISTING reads over the full filtered id set — the census
    // recall-stats fan-out (`census_recall_stats`, the very fetch the
    // default path runs page-scoped below) and `reading_latest_for_ids`
    // (the SAME source recall's `read_pct` enrichment reads, so the census
    // bucket and the SPA's CT-B6 tension badge agree row-for-row) — then
    // reuses the stats map for the page projection (no second fetch). The
    // default path is untouched: page-scoped stats, id-ASC order,
    // byte-identical responses.
    let mut full_recall_stats: Option<CensusRecallStats> = None;
    if sort_unverified {
        let all_ids: Vec<String> = memories.iter().map(|d| d.id.clone()).collect();
        let stats = census_recall_stats(&state, kb_name.as_str(), all_ids.clone()).await;
        // "Never opened by you" is per-user (this request's resolved
        // identity, invariant #4's ladder), like every reading-progress
        // read (#19). A kb with reading capture off simply has no visit
        // rows — every row honestly reads never-opened, exactly like the
        // SPA's absent `read_pct`.
        let latest = if ctx.reading_progress {
            ctx.storage
                .reading_latest_for_ids(all_ids, identity.user.clone())
                .await
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        memories.sort_by(|a, b| {
            unverified_cmp(
                (
                    a.id.as_str(),
                    stats.get(&a.id).map(|s| s.0).unwrap_or(0),
                    never_opened_by_human(latest.get(&a.id)),
                ),
                (
                    b.id.as_str(),
                    stats.get(&b.id).map(|s| s.0).unwrap_or(0),
                    never_opened_by_human(latest.get(&b.id)),
                ),
            )
        });
        full_recall_stats = Some(stats);
    }

    let page: Vec<_> = memories
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();

    let pinned = ctx.storage.pinned_memories_set().await.unwrap_or_default();
    let links = ctx.storage.memory_links_all().await.unwrap_or_default();

    // Recall-usage fan-out (invariant #28): every kb's `memory_recalls`
    // table MAY hold rows naming (this kb, one of this page's ids) — sum
    // count, take the max last_recalled_at, across the per-kb partials.
    // CT-E4 — when ?sort=unverified already fetched the stats for every
    // filtered id (a strict superset of this page), reuse that map.
    let recall_stats: CensusRecallStats = match full_recall_stats {
        Some(stats) => stats,
        None => {
            let ids: Vec<String> = page.iter().map(|d| d.id.clone()).collect();
            census_recall_stats(&state, kb_name.as_str(), ids).await
        }
    };

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rows: Vec<CensusRow> = page
        .into_iter()
        .map(|d| {
            let raw_links = links.get(&d.id);
            let global = raw_links.map(|s| s.contains("*")).unwrap_or(false);
            let mut linked_kbs: Vec<String> = raw_links
                .map(|s| s.iter().filter(|k| k.as_str() != "*").cloned().collect())
                .unwrap_or_default();
            linked_kbs.sort();
            let (recall_count, last_recalled_at, recall_used_count) =
                recall_stats.get(&d.id).cloned().unwrap_or((0, None, 0));
            let created = d.created_unix.or(d.mtime_unix);
            let age_days = created.map(|c| (now_unix - c).max(0) as f32 / 86400.0);
            // MI-W2.3 — the tombstone flag. Read before `d.kb_category` etc
            // move their fields out below (a different field, so this is
            // just an ordinary borrow, not fighting the partial move).
            let forgotten = d.kb_status.as_deref() == Some("forgotten");
            // CT-C3 — same borrow-before-move treatment as `forgotten`:
            // read the tag off `d.tags` before it moves into the row below.
            let failed = kb_core::memory::has_failed_outcome_tag(&d.tags);
            CensusRow {
                id: d.id.clone(),
                title: d.title,
                category: d.kb_category,
                salience: d.kb_salience,
                decay_bucket: d.kb_decay,
                created_unix: created,
                age_days,
                pinned: pinned.contains(&d.id),
                global,
                linked_kbs,
                forgotten,
                failed,
                supersedes: d.kb_supersedes,
                superseded_by: superseded_by.get(&d.id).cloned(),
                tags: d.tags,
                session_id: d.kb_session,
                recall_count,
                last_recalled_at,
                recall_used_count,
                memory_type: d.kb_memory_type,
                source: d.kb_source,
                author: d.kb_author,
                source_kb: d.kb_source_kb,
                source_anchor: parse_source_anchor(d.kb_source_anchor.as_deref()),
                source_artifact: d.kb_source_artifact,
            }
        })
        .collect();

    Json(CensusResponse {
        rows,
        total,
        offset,
        limit,
    })
    .into_response()
}

/// Census recall-stats fan-out (invariant #28): every kb's `memory_recalls`
/// ledger MAY hold rows naming (`target_kb`, one of `ids`) — sum `count`/
/// `used_count`, take the max `last_recalled_at`, across the per-kb
/// partials. CT-C5 — the third tuple slot (`used_count`) is DISPLAY-only,
/// never fed into any scoring path (see `CensusRow::recall_used_count`).
/// Extracted from the `census` handler so CT-E4's pre-pagination sort fetch
/// (every filtered id) and the default page-scoped fetch share one
/// implementation.
async fn census_recall_stats(
    state: &KbHandles,
    target_kb: &str,
    ids: Vec<String>,
) -> CensusRecallStats {
    let target_kb = target_kb.to_string();
    let mut futs: Vec<super::CorpusFut<'_, Vec<kb_core::storage::sqlite::MemoryRecallCount>>> =
        Vec::new();
    for (kb2, ctx2) in state.kbs.iter() {
        let ids = ids.clone();
        let target_kb = target_kb.clone();
        futs.push(Box::pin(async move {
            ctx2.storage
                .memory_recalls_counts_for_ids(Some(target_kb), ids)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb2, error = %e, "memory_recalls_counts_for_ids failed");
                    Vec::new()
                })
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let mut recall_stats: CensusRecallStats = HashMap::new();
    for part in partials {
        for c in part {
            let e = recall_stats.entry(c.memory_id).or_insert((0, None, 0));
            e.0 = e.0.saturating_add(c.count);
            e.1 = match (e.1, c.last_recalled_at) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) => Some(a),
                (None, b) => b,
            };
            e.2 = e.2.saturating_add(c.used_count);
        }
    }
    recall_stats
}

/// CT-E4 — "never opened by you": no latest-visit row at all OR a recorded
/// visit that never scrolled (`pct == 0`). EXACTLY the SPA condition
/// CT-B6's dossier Attention section pinned (`read_pct` absent/0 both mean
/// "never meaningfully opened" — `web/src/lib/attention.ts`), so the census
/// bucket and the SPA tension badge can never disagree on a row.
fn never_opened_by_human(latest: Option<&(u8, Option<String>, i64)>) -> bool {
    latest.map(|(pct, _, _)| *pct == 0).unwrap_or(true)
}

/// CT-E4 — the `?sort=unverified` comparator over `(id, recall_count,
/// never_opened)`: agent-hot-human-cold rows first (the drift meter between
/// fleet belief-updates and operator attention) — the (recall_count > 0 AND
/// never opened) BUCKET leads, recall_count DESC orders within/after it,
/// and id ASC (the census's default paging order) breaks ties, so paging
/// stays deterministic and a signal-less corpus degrades to the default
/// order exactly. SURFACED-NEVER-SCORED (#10's CT posture): this orders a
/// census LISTING only; nothing here is reachable from `kb_core::memory`'s
/// scoring types.
fn unverified_cmp(a: (&str, u32, bool), b: (&str, u32, bool)) -> std::cmp::Ordering {
    fn key(recall_count: u32, never_opened: bool) -> (bool, std::cmp::Reverse<u32>) {
        let agent_hot_human_cold = recall_count > 0 && never_opened;
        (!agent_hot_human_cold, std::cmp::Reverse(recall_count))
    }
    key(a.1, a.2).cmp(&key(b.1, b.2)).then_with(|| a.0.cmp(b.0))
}

// === CT-A1 (U3 parse-back) — reverse provenance ============================

const MEMORIES_FROM_LIMIT: u32 = 50;

/// One memory that was highlighted FROM a given artifact — the reverse of
/// `MemoryProvenance`.
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct MemoryFromRow {
    pub id: String,
    pub kb: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub summary: Option<String>,
    /// The `you`/`claude` role this memory was highlighted under.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub author: Option<String>,
    /// Best-effort parsed `review::Anchor` — see `parse_source_anchor`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub anchor: Option<kb_core::review::Anchor>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub created_unix: Option<i64>,
}

#[cfg_attr(feature = "ts-export", derive(ts_rs::TS), ts(export))]
#[derive(Debug, Serialize)]
pub struct MemoriesFromResponse {
    pub rows: Vec<MemoryFromRow>,
}

/// `GET /api/kb/{kb}/docs/{id}/memories-from` — every memory that was
/// highlighted FROM the artifact `{kb}/{id}` (the reverse of
/// `MemoryProvenance`). A highlight-born memory almost always lives in a
/// DIFFERENT corpus than the artifact it was lifted from, so this fans out
/// across every memory-scoped corpus on the daemon (invariant #28,
/// `buffered_join`, `BTreeMap` submission order); one corpus's storage error
/// is logged and dropped, never a 500 for the whole response.
pub async fn memories_from(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, _ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid doc id {id:?}"
        )));
    }
    let source_kb = kb_name.as_str().to_string();

    let mut futs: Vec<super::CorpusFut<'_, Vec<(String, kb_core::storage::lance::DocSummary)>>> =
        Vec::new();
    for (kb2, ctx2) in state.kbs.iter() {
        if ctx2.memory_scope.is_none() {
            continue;
        }
        let kb2_str = kb2.as_str().to_string();
        let source_kb = source_kb.clone();
        let id = id.clone();
        futs.push(Box::pin(async move {
            let rows = ctx2
                .storage
                .list_docs_with_kb_source_artifact(source_kb, id, MEMORIES_FROM_LIMIT)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        kb = %kb2_str,
                        error = %e,
                        "list_docs_with_kb_source_artifact failed"
                    );
                    Vec::new()
                });
            rows.into_iter()
                .map(|d| (kb2_str.clone(), d))
                .collect::<Vec<_>>()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let rows: Vec<MemoryFromRow> = partials
        .into_iter()
        .flatten()
        .map(|(kb2, d)| MemoryFromRow {
            id: d.id,
            kb: kb2,
            title: d.title,
            summary: d.kb_summary,
            author: d.kb_author,
            anchor: parse_source_anchor(d.kb_source_anchor.as_deref()),
            created_unix: d.created_unix.or(d.mtime_unix),
        })
        .collect();

    Json(MemoriesFromResponse { rows }).into_response()
}

// === MI-W2.4a — supersede-chain lineage ====================================

/// One node on a lineage chain — enough for `kb memory log` to render a hop
/// (id/title/created + the tombstone flag) AND (MI-W4.3) for the SPA
/// lineage viewer's per-node inline decay sparkline.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryLineageNode")
)]
#[derive(Debug, Serialize)]
pub struct LineageNode {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub created_unix: Option<i64>,
    /// MI-W2.3 — `true` when this hop is itself soft-forgotten.
    pub forgotten: bool,
    /// MI-W4.3 — same decay-projection ingredients as `RecallResult`
    /// (`salience`/`decay_k`/`age_days`), so the lineage viewer's per-node
    /// sparkline reuses the SAME `web/src/lib/decayProjection.ts` module
    /// rather than a third implementation. `None` only when the node has no
    /// resolvable `created`/`mtime` at all (genuinely undecayed).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub salience: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub decay_k: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub age_days: Option<f32>,
    /// MI-W4.3 — pinned nodes are exempt from the decay floor; the viewer
    /// renders them distinctly instead of a crossing label.
    #[serde(default)]
    pub pinned: bool,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryLineageResponse")
)]
#[derive(Debug, Serialize)]
pub struct LineageResponse {
    pub id: String,
    /// The requested memory itself, rendered as a node so the caller
    /// doesn't need a second fetch for its title/created/forgotten-state.
    pub start: LineageNode,
    /// Walking `kb_supersedes` FORWARD from `id`: what this memory
    /// replaces, then what THAT replaced, and so on. Oldest last.
    pub supersedes_chain: Vec<LineageNode>,
    /// Walking the REVERSE pointer: what superseded this memory, then what
    /// superseded THAT, and so on. Newest last. When a hop is ambiguous
    /// (more than one memory claims to supersede the same predecessor —
    /// nothing at write time prevents that), the walk follows the
    /// deterministically-sorted first match (see
    /// `storage::lance::Storage::find_superseded_by`); it does not fork.
    pub superseded_by_chain: Vec<LineageNode>,
}

/// Bound on chain length in EITHER direction — a cycle (a corrupted or
/// adversarially-authored `kb-supersedes` ring) must terminate the walk
/// rather than loop forever; a `seen` set also catches a cycle short of
/// this bound.
const LINEAGE_MAX_HOPS: usize = 200;

fn lineage_node(
    d: &kb_core::storage::lance::DocSummary,
    now_unix: i64,
    pinned: &HashSet<String>,
) -> LineageNode {
    let created = d.created_unix.or(d.mtime_unix);
    let age_days = created.map(|c| (now_unix - c).max(0) as f32 / 86_400.0);
    LineageNode {
        id: d.id.clone(),
        title: d.title.clone(),
        created_unix: created,
        forgotten: d.kb_status.as_deref() == Some("forgotten"),
        salience: d.kb_salience,
        decay_k: Some(kb_core::memory::decay_k(d.kb_decay.as_deref())),
        age_days,
        pinned: pinned.contains(&d.id),
    }
}

/// `GET /api/kb/{kb}/memories/{id}/lineage` — MI-W2.4a: `kb memory log`'s
/// data source. Walks one supersede chain in BOTH directions from `id`:
/// what it supersedes (forward, via its own `kb_supersedes`) and what
/// superseded it (reverse, via `find_superseded_by`). Each hop is
/// resolved through the read-lane `lineage_by_id`/`find_superseded_by`
/// storage messages (invariant SC4) — a lineage walk is a search-adjacent
/// read, not a mutation. 404 when `id` itself doesn't exist in this kb.
pub async fn lineage(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let start = match ctx.storage.lineage_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // MI-W4.3 — pinned set, so the lineage viewer can render a pinned hop
    // distinctly instead of a crossing label. Best-effort: a read failure
    // just means no hop in this response resolves as pinned (matches the
    // recall route's own X1 fallback posture for the same read).
    let pinned = ctx.storage.pinned_memories_set().await.unwrap_or_default();

    // Forward — follow `kb_supersedes` from `start`. `seen` guards against
    // a cycle independent of `LINEAGE_MAX_HOPS`.
    let mut supersedes_chain = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([id.clone()]);
    let mut cursor = start.kb_supersedes.clone();
    while let Some(next_id) = cursor {
        if supersedes_chain.len() >= LINEAGE_MAX_HOPS || !seen.insert(next_id.clone()) {
            break;
        }
        match ctx.storage.lineage_by_id(next_id).await {
            Ok(Some(d)) => {
                cursor = d.kb_supersedes.clone();
                supersedes_chain.push(lineage_node(&d, now_unix, &pinned));
            }
            _ => break,
        }
    }

    // Reverse — repeatedly ask "who claims to supersede THIS id", starting
    // from `id` itself. `find_superseded_by` already sorts a genuine fork
    // deterministically; this walk follows the first not-yet-seen match.
    let mut superseded_by_chain = Vec::new();
    let mut seen_rev: HashSet<String> = HashSet::from([id.clone()]);
    let mut cursor_id = id.clone();
    while superseded_by_chain.len() < LINEAGE_MAX_HOPS {
        let matches = ctx
            .storage
            .find_superseded_by(cursor_id.clone())
            .await
            .unwrap_or_default();
        let Some(next) = matches.into_iter().find(|d| seen_rev.insert(d.id.clone())) else {
            break;
        };
        cursor_id = next.id.clone();
        superseded_by_chain.push(lineage_node(&next, now_unix, &pinned));
    }

    let start_node = lineage_node(&start, now_unix, &pinned);
    Json(LineageResponse {
        id,
        start: start_node,
        supersedes_chain,
        superseded_by_chain,
    })
    .into_response()
}

// === CT-B2 — recalled-by ====================================================

/// Total row cap across every fanned-out kb (`GET
/// /api/kb/{kb}/memories/{id}/recalled-by`) — a memory recalled thousands
/// of times is a display problem, not a data one; the response is a
/// best-effort census, not a complete log (see the route's own doc
/// comment).
const RECALLED_BY_LIMIT: u32 = 200;

/// One recalling session on a memory's recall history — the memory-side
/// reverse of `GET /api/sessions/{sid}/recalls`. Labelled "recalls the
/// capture pipeline saw" wherever it's rendered (CLI/SPA): a best-effort
/// census over each kb's own `memory_recalls` ledger, never a complete
/// injection log — a hook that misfired, a session never captured, or a
/// row not yet re-indexed all leave silent gaps.
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryRecalledByRow")
)]
#[derive(Debug, Serialize)]
pub struct RecalledByRow {
    /// The kb whose `memory_recalls` table this row came from — the
    /// RECALLING session's own corpus (invariant #28's fan-out target),
    /// not necessarily this memory's `{kb}`.
    pub session_kb: String,
    pub session_id: String,
    /// The recalling session's own display name (`title` → non-empty
    /// `first_user_prompt`, else absent) — never a third "session
    /// <short-id>" rung; the caller falls back to `session_id` itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub session_title: Option<String>,
    /// The session-view/1 `t-<uuid12>` Turn id the injection landed on,
    /// when the parse recovered one (see `derive_memory_recalls`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub recalled_at: Option<i64>,
    /// CT-C5 (V0037) — did the recalling session go on to EXPLICITLY name
    /// this memory (id or ≥12-char title, verbatim) in a later turn?
    /// Explicit-reference-only: an agent can act on a fact without naming
    /// it, and that reads as `false` here — label accordingly wherever
    /// rendered. Absent-when-false on the wire.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    #[cfg_attr(feature = "ts-export", ts(as = "Option<bool>", optional))]
    pub used: bool,
    /// MR1 (V0040) — the hit's RANK in the pack that injected it, 1 = top.
    /// Absent when the capture's `kb-recall/1` marker carried no `pos=`
    /// (a pre-MR1 capture, a fallback-only parse, or a mangled value); see
    /// `kb_core::sessions::view::DerivedRecall::pos` for why the hit's
    /// position in the transcript is deliberately never used as a
    /// substitute. SURFACED-NEVER-SCORED — no ranking path can read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub pos: Option<u32>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryRecalledByResponse")
)]
#[derive(Debug, Serialize)]
pub struct RecalledByResponse {
    pub rows: Vec<RecalledByRow>,
}

/// `GET /api/kb/{kb}/memories/{id}/recalled-by` — every session that
/// recalled this memory: the memory-side reverse of the `memory_recalls`
/// ledger (`GET /api/sessions/{sid}/recalls` is the session-side view of
/// the same table). The ledger lives with the RECALLING session's own kb
/// — typically the sessions corpus, NOT this memory's `{kb}` — so this
/// fans out across EVERY kb on the daemon (invariant #28, `buffered_join`,
/// `BTreeMap` submission order); one corpus's storage error is logged and
/// dropped, never a 500 for the whole response. `{kb}`/`{id}` are only
/// used as the `memory_kb`/`memory_id` filter values — this route does
/// NOT require the memory to still exist in `{kb}` (a forgotten/deleted
/// memory's recall history stays readable, matching `lineage`'s and
/// `memories_from`'s own best-effort posture).
pub async fn recalled_by(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, _ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid memory id {id:?}"
        )));
    }
    let memory_kb = kb_name.as_str().to_string();

    let mut futs: Vec<
        super::CorpusFut<'_, Vec<(String, kb_core::storage::sqlite::MemoryRecalledByRow)>>,
    > = Vec::new();
    for (kb2, ctx2) in state.kbs.iter() {
        let kb2_str = kb2.as_str().to_string();
        let memory_kb = memory_kb.clone();
        let id = id.clone();
        futs.push(Box::pin(async move {
            let rows = ctx2
                .storage
                .memory_recalls_for_memory(memory_kb, id, RECALLED_BY_LIMIT)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb2_str, error = %e, "memory_recalls_for_memory failed");
                    Vec::new()
                });
            rows.into_iter()
                .map(|r| (kb2_str.clone(), r))
                .collect::<Vec<_>>()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let mut rows: Vec<RecalledByRow> = partials
        .into_iter()
        .flatten()
        .map(|(session_kb, r)| RecalledByRow {
            session_kb,
            session_id: r.session_id,
            session_title: r
                .title
                .filter(|t| !t.trim().is_empty())
                .or_else(|| r.first_user_prompt.filter(|p| !p.trim().is_empty())),
            turn_id: r.turn_id,
            recalled_at: r.recalled_at,
            used: r.used,
            pos: r.pos,
        })
        .collect();
    // Merge-sort across kbs: newest `recalled_at` first (absent sorts
    // last — `Option`'s derived `Ord` ranks `None < Some`, so comparing
    // `b` against `a` puts the larger/`Some` side first and `None` last),
    // tie-broken deterministically by `(session_kb, session_id)` — each
    // per-kb partial already arrives sorted from
    // `memory_recalls_for_memory`, but the fan-out itself is submission-
    // (kb-name-)ordered, not recall-time-ordered.
    rows.sort_by(|a, b| {
        b.recalled_at
            .cmp(&a.recalled_at)
            .then_with(|| a.session_kb.cmp(&b.session_kb))
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    rows.truncate(RECALLED_BY_LIMIT as usize);

    Json(RecalledByResponse { rows }).into_response()
}

// === CT-F1 — committed-in (the memory↔commit exact-id join) =================

/// Total row cap across every fanned-out kb (`GET
/// /api/kb/{kb}/memories/{id}/commits`). Smaller than
/// [`RECALLED_BY_LIMIT`] on purpose: a recall is an event that repeats
/// every session, while a CITATION is a deliberate act — a memory cited in
/// more than 50 commits is a display problem long before it's a data one.
const COMMITTED_IN_LIMIT: u32 = 50;

/// CT-F1 — one commit that cited this memory via a `Kb-Memory:` trailer.
/// EXACT-ID trust: unlike the session→commits chain (`kb why-memory`'s
/// heuristic hop, which says "the session that produced this memory also
/// produced these commits"), every row here is a commit that NAMED this
/// memory's id in its own message. What it still does NOT claim: that the
/// memory is why the commit is correct, or that the commit is still in
/// history (nothing re-checks a rebase).
#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryCommittedInRow")
)]
#[derive(Debug, Serialize)]
pub struct CommittedInRow {
    /// The kb whose `memory_commits` table this row came from — the
    /// RECORDING session's own corpus (invariant #28's fan-out target),
    /// not necessarily this memory's `{kb}`.
    pub session_kb: String,
    /// The session whose capture carried the trailer.
    pub session_id: String,
    /// Full commit sha (capture-time `git show`).
    pub sha_full: String,
    /// Short sha as the transcript detected it, when it had one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub subject: Option<String>,
    /// Which repo the sha lives in — a bare sha is meaningless across
    /// repos, so this is load-bearing for honesty, never a path kb opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub repo_root: Option<String>,
    /// When the row was DERIVED (the capture's indexer clock) — NOT the
    /// commit's author/commit date. The capture envelope carries no commit
    /// timestamp and CT-F1 refuses a second git read to invent one.
    pub recorded_at: i64,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryCommittedInResponse")
)]
#[derive(Debug, Serialize)]
pub struct CommittedInResponse {
    pub rows: Vec<CommittedInRow>,
}

/// `GET /api/kb/{kb}/memories/{id}/commits` — CT-F1: every commit that
/// cited this memory by id in a `Kb-Memory:` trailer. The rows live with
/// the RECORDING session's own kb (typically the sessions corpus, NOT this
/// memory's `{kb}`), exactly like `memory_recalls`, so this fans out across
/// EVERY kb on the daemon (invariant #28, `buffered_join`, `BTreeMap`
/// submission order); one corpus's storage error is logged and dropped,
/// never a 500 for the whole response.
///
/// `{kb}`/`{id}`: `{kb}` is validated but NOT used as a filter — the
/// trailer grammar carries no kb name (`Kb-Memory: <hex12>`), so the join
/// key is the id alone (the id-collision caveat is on the V0038 migration
/// header). Like `recalled_by`/`lineage`, this does not require the memory
/// to still exist in `{kb}`: a forgotten memory's citation history stays
/// readable.
///
/// **An empty result is a NON-SIGNAL.** The `Kb-Memory:` trailer is
/// opt-in per repo and OFF by default, so "no rows" almost always means
/// "that repo never opted in", never "this memory influenced nothing".
/// Every renderer must label it that way.
pub async fn committed_in(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, _ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    if !crate::routes::is_safe_id(&id) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "invalid memory id {id:?}"
        )));
    }

    let mut futs: Vec<
        super::CorpusFut<'_, Vec<(String, kb_core::storage::sqlite::MemoryCommitRow)>>,
    > = Vec::new();
    for (kb2, ctx2) in state.kbs.iter() {
        let kb2_str = kb2.as_str().to_string();
        let id = id.clone();
        futs.push(Box::pin(async move {
            let rows = ctx2
                .storage
                .memory_commits_for_memory(id, COMMITTED_IN_LIMIT)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(kb = %kb2_str, error = %e, "memory_commits_for_memory failed");
                    Vec::new()
                });
            rows.into_iter()
                .map(|r| (kb2_str.clone(), r))
                .collect::<Vec<_>>()
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let partials = super::buffered_join(futs, state.fanout_cap).await;
    let mut rows: Vec<CommittedInRow> = partials
        .into_iter()
        .flatten()
        .map(|(session_kb, r)| CommittedInRow {
            session_kb,
            session_id: r.session_id,
            sha_full: r.sha_full,
            sha: r.sha,
            subject: r.subject,
            repo_root: r.repo_root,
            recorded_at: r.recorded_at,
        })
        .collect();
    // Merge-sort across kbs: newest first, tie-broken deterministically by
    // `(session_kb, sha_full)`. Each per-kb partial already arrives sorted
    // from `memory_commits_for_memory`, but the fan-out itself is
    // submission- (kb-name-)ordered, not time-ordered.
    rows.sort_by(|a, b| {
        b.recorded_at
            .cmp(&a.recorded_at)
            .then_with(|| a.session_kb.cmp(&b.session_kb))
            .then_with(|| a.sha_full.cmp(&b.sha_full))
    });
    // The same sha can legitimately appear in TWO corpora's tables (two
    // sessions corpora capturing overlapping work); within ONE kb the
    // `(memory_id, sha_full)` PK already makes that impossible. One commit
    // is one citation, so keep the first (newest) occurrence — `retain` on
    // a seen-set, NOT `dedup_by`, which only collapses ADJACENT rows and
    // would miss two kbs whose `recorded_at` differ.
    let mut seen_sha: std::collections::HashSet<String> = std::collections::HashSet::new();
    rows.retain(|r| seen_sha.insert(r.sha_full.clone()));
    rows.truncate(COMMITTED_IN_LIMIT as usize);

    Json(CommittedInResponse { rows }).into_response()
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "TombstoneEraResponse")
)]
#[derive(Debug, Serialize)]
pub struct TombstoneEraResponse {
    /// MI-W2.4c — unix seconds this daemon first became capable of
    /// soft-forgetting (MI-W2.3) rather than hard-deleting with no trace.
    /// A temporal query (`kb memory log`, `kb diff --between`) whose
    /// requested window starts before this MUST print an explicit caveat
    /// — anything deleted before it is genuinely unrecoverable and
    /// undetectable, not merely "not found".
    pub started_unix: i64,
}

/// `GET /api/memory/tombstone-era` — MI-W2.4c EPOCH HONESTY marker.
pub async fn tombstone_era(State(state): State<Arc<KbHandles>>) -> Json<TombstoneEraResponse> {
    Json(TombstoneEraResponse {
        started_unix: state.tombstone_era_started_unix,
    })
}

// === v0.10 M2 — pinning + decay policy ====================================

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryPinResponse")
)]
#[derive(Debug, Serialize)]
pub struct PinResponse {
    pub pinned: bool,
}

/// POST /api/kb/{kb}/memories/{id}/pin — mark a memory as pinned.
/// Idempotent; returns the current pin state. 404 if the kb/artifact
/// id doesn't exist.
pub async fn pin_memory(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response<Body> {
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // Don't pin a nonexistent artifact — same reasoning as the
    // corkboard route. Typos surface as 404 rather than dangling rows.
    match ctx.storage.get_by_id(artifact_id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {artifact_id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    }
    let now_unix = chrono::Utc::now().timestamp();
    match ctx.storage.pinned_memory_add(artifact_id, now_unix).await {
        Ok(_) => Json(PinResponse { pinned: true }).into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

/// DELETE /api/kb/{kb}/memories/{id}/pin — unpin. 204; idempotent.
pub async fn unpin_memory(
    State(state): State<Arc<KbHandles>>,
    Path((kb, artifact_id)): Path<(String, String)>,
) -> Response<Body> {
    let (_kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    match ctx.storage.pinned_memory_remove(artifact_id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error_to_problem_json(&e),
    }
}

// === MI-W3.2b — salience edit ==============================================

#[derive(Debug, Deserialize)]
pub struct SaliencePatchBody {
    pub salience: f32,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemorySalienceResponse")
)]
#[derive(Debug, Serialize)]
pub struct SalienceResponse {
    pub id: String,
    pub salience: f32,
}

/// `PATCH /api/kb/{kb}/memories/{id}/salience` — MI-W3.2b: edit ONLY a
/// memory's salience, in its own source. Memory metadata is otherwise
/// IMMUTABLE via the API (`patch_meta` is scoped strictly to
/// `kb-tags`/`kb-category`, see its doc comment) — this is a deliberate,
/// narrow exception the W4 hygiene queue and human triage both need, so it
/// gets its OWN route rather than widening `patch_meta`'s scope.
///
/// Splices `kb-salience` via [`kb_core::memory::set_salience`] (the same
/// generic byte-preserving meta editors MI-W2.3's soft-forget uses), then
/// lets the watcher reindex — the "write-only, searchable within one
/// debounce" path every other source edit takes. `salience` is clamped to
/// `[0,1]`, matching `render_artifact`'s write-time clamp; a non-finite
/// value (NaN/inf) 400s rather than being silently clamped into something
/// that could re-enter as a false "0" or "1".
pub async fn patch_salience(
    State(state): State<Arc<KbHandles>>,
    Path((kb, id)): Path<(String, String)>,
    Json(body): Json<SaliencePatchBody>,
) -> Response<Body> {
    if !body.salience.is_finite() {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "salience must be a finite number".into(),
        ));
    }
    let salience = body.salience.clamp(0.0, 1.0);
    let (kb_name, ctx) = match crate::routes::resolve_kb(&state, &kb) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let doc = match ctx.storage.get_by_id(id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {id} in kb {kb_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    let src = match std::fs::read_to_string(&doc.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact source {} missing on disk",
                doc.path
            )))
        }
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                doc.path
            )))
        }
    };
    let is_md = ctx.ext_map.is_markdown(std::path::Path::new(&doc.path));
    let new_src = kb_core::memory::set_salience(&src, is_md, salience);
    if new_src != src {
        if let Err(e) =
            kb_core::fsx::write_atomic(std::path::Path::new(&doc.path), new_src.as_bytes())
        {
            return error_to_problem_json(&e);
        }
    }
    // No SSE emit here — same reasoning as `patch_meta`: the watcher's
    // reindex fires the authoritative `artifact.indexed` within one
    // debounce; the PATCH response gives the editing client instant
    // optimistic feedback.
    Json(SalienceResponse { id, salience }).into_response()
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryPolicyBody")
)]
#[derive(Debug, Serialize, Deserialize)]
pub struct PolicyBody {
    pub policy: String,
    /// MI-W4.1 — the ACTIVE policy's salience drop threshold
    /// (`DecayPolicy::drop_threshold()`), so a client (the `/memory`
    /// health-timeline sparkline's reference line) never has to duplicate
    /// the three-way strict/balanced/loose mapping. `None` represents
    /// `Loose` (no floor — nothing crosses, ever); a real `f32::NEG_INFINITY`
    /// is deliberately never put on the wire (JSON has no `Infinity`
    /// literal). `#[serde(default)]` so an OLDER client's bare `{"policy":
    /// "strict"}` `PUT` body — this struct doubles as the request type —
    /// still deserializes; the field is ignored on `PUT` regardless (only
    /// `policy` is read) and always populated fresh on the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub drop_threshold: Option<f32>,
}

/// The active policy's threshold, wire-safe (`Loose`'s `NEG_INFINITY` →
/// `None` rather than a non-finite JSON number).
fn wire_drop_threshold(policy: DecayPolicy) -> Option<f32> {
    let t = policy.drop_threshold();
    t.is_finite().then_some(t)
}

/// GET /api/memory/policy — current daemon-wide decay policy.
pub async fn get_policy(State(state): State<Arc<KbHandles>>) -> Json<PolicyBody> {
    let p = *state.memory_policy.read().await;
    Json(PolicyBody {
        policy: p.as_str().to_string(),
        drop_threshold: wire_drop_threshold(p),
    })
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryPromoteBody")
)]
#[derive(Debug, Deserialize)]
pub struct PromoteBody {
    pub dest_kb: String,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryPromoteResponse")
)]
#[derive(Debug, Serialize)]
pub struct PromoteResponse {
    pub id: String,
    pub path: String,
    pub kb: String,
}

/// v0.13 D7 — POST /api/kb/{src_kb}/memories/{id}/promote
/// `{"dest_kb": "research"}` → copy a memory artifact into a different
/// (non-memory) kb, stripping the memory-specific metas
/// (`kb-salience`, `kb-decay`, `kb-pinned`, `kb-supersedes`). The
/// indexer picks the new file up on its filesystem watcher pass.
///
/// 404 if the source artifact or dest kb is unknown; 400 if the dest
/// kb is itself a memory corpus (the design intent is "promote OUT of
/// memory, never sideways").
pub async fn promote(
    State(state): State<Arc<KbHandles>>,
    Path((src_kb, artifact_id)): Path<(String, String)>,
    Json(body): Json<PromoteBody>,
) -> Response<Body> {
    let src_name = match KbName::new(&src_kb) {
        Ok(k) => k,
        Err(e) => return error_to_problem_json(&e),
    };
    let dest_name = match KbName::new(&body.dest_kb) {
        Ok(k) => k,
        Err(e) => return error_to_problem_json(&e),
    };
    if src_name == dest_name {
        return error_to_problem_json(&kb_core::Error::BadRequest(
            "promote: dest_kb must differ from src_kb".into(),
        ));
    }
    let Some(src_ctx) = state.kbs.get(&src_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {src_name}")));
    };
    let Some(dest_ctx) = state.kbs.get(&dest_name) else {
        return error_to_problem_json(&kb_core::Error::NotFound(format!("kb {dest_name}")));
    };
    if dest_ctx.memory_scope.is_some() {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "dest_kb {dest_name} is a memory corpus — promote must target a regular kb"
        )));
    }
    let src_doc = match src_ctx.storage.get_by_id(artifact_id.clone()).await {
        Ok(Some(d)) => d,
        Ok(None) => {
            return error_to_problem_json(&kb_core::Error::NotFound(format!(
                "artifact {artifact_id} in kb {src_name}"
            )))
        }
        Err(e) => return error_to_problem_json(&e),
    };
    // Read the memory's HTML from disk, strip memory metas, write to
    // the dest kb's source root with a fresh collision-safe filename
    // (mirrors the artifacts ingest route's slug-then-timestamp pattern).
    let raw = match std::fs::read_to_string(&src_doc.path) {
        Ok(s) => s,
        Err(e) => {
            return error_to_problem_json(&kb_core::Error::Storage(format!(
                "read {}: {e}",
                src_doc.path
            )))
        }
    };
    let promoted = kb_core::memory::strip_memory_metas(&raw);

    // Filename: <slug>-<unix>[.html]. Collision-safe — slug + unix
    // second is sufficient outside the deepest pathological cases.
    let slug = kb_core::memory::memory_slug(&src_doc.title);
    let unix = chrono::Utc::now().timestamp();
    let dest_filename = format!("{slug}-{unix}.html");
    let dest_path = dest_ctx.source_path.join(&dest_filename);
    if let Err(e) = std::fs::write(&dest_path, &promoted) {
        return error_to_problem_json(&kb_core::Error::Storage(format!(
            "write {}: {e}",
            dest_path.display()
        )));
    }
    // Compute the new artifact id from the destination path so the
    // SPA can navigate to it without waiting for the indexer (the
    // watcher picks it up on its debounce; the response is enough
    // for the modal to surface a "promoted to →" link).
    let new_id = kb_core::ids::ArtifactId::from_path(
        &dest_path
            .strip_prefix(&dest_ctx.source_path)
            .unwrap_or(&dest_path)
            .to_string_lossy(),
    );
    src_ctx.bus.emit(
        "memory.promoted",
        serde_json::json!({
            "src_kb": src_name.as_str(),
            "dest_kb": dest_name.as_str(),
            "src_id": artifact_id,
            "dest_id": new_id.as_str(),
            "dest_path": dest_filename,
        }),
    );
    Json(PromoteResponse {
        id: new_id.as_str().to_string(),
        path: dest_filename,
        kb: dest_name.to_string(),
    })
    .into_response()
}

// === MI-W3.1 — cross-corpus duplicate report ===============================

const DUPES_DEFAULT_LIMIT: usize = 50;
const DUPES_MAX_LIMIT: usize = 200;

#[derive(Debug, Deserialize)]
pub struct DupesParams {
    /// Similarity floor — see `kb_core::memory::find_duplicate_pairs` for
    /// why the default is 0.90.
    pub threshold: Option<f32>,
    pub limit: Option<usize>,
    /// Restrict the scan to ONE memory corpus (disables cross-corpus
    /// comparison entirely — there's only one corpus in scope). Absent ⇒
    /// every memory corpus on the daemon (`memory_scope` set), the
    /// cross-corpus case this report exists for.
    pub kb: Option<String>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryDupePair")
)]
#[derive(Debug, Serialize)]
pub struct DupePairOut {
    pub kb_a: String,
    pub id_a: String,
    pub title_a: String,
    pub kb_b: String,
    pub id_b: String,
    pub title_b: String,
    pub cosine: f32,
    pub cross_corpus: bool,
}

impl From<kb_core::memory::DupePair> for DupePairOut {
    fn from(p: kb_core::memory::DupePair) -> Self {
        DupePairOut {
            kb_a: p.kb_a,
            id_a: p.id_a,
            title_a: p.title_a,
            kb_b: p.kb_b,
            id_b: p.id_b,
            title_b: p.title_b,
            cosine: p.cosine,
            cross_corpus: p.cross_corpus,
        }
    }
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryDupesResponse")
)]
#[derive(Debug, Serialize)]
pub struct DupesResponse {
    pub pairs: Vec<DupePairOut>,
    /// The threshold actually used (the request's, or the default).
    pub threshold: f32,
    /// How many live (not forgotten, embedding present) candidate memories
    /// were considered — lets a caller sanity-check "0 pairs" against
    /// "0 candidates" vs. "372 candidates, genuinely nothing over
    /// threshold."
    pub scanned: u32,
}

/// `GET /api/memory/dupes[?threshold=][&limit=][&kb=]` — MI-W3.1: an
/// ON-DEMAND report of likely-redundant memory PAIRS (high embedding
/// similarity, not already linked by `kb-supersedes` in either direction,
/// neither forgotten), fanned out across every memory corpus on the daemon
/// (invariant #28 — `buffered_join`, submission order, drop-on-error per
/// corpus). Deliberately NOT a hook: no auto-generated review comments, no
/// alert fatigue — see `kb_core::memory::find_duplicate_pairs`'s doc
/// comment for the full "why a report, not a detector" reasoning and the
/// threshold justification. NEVER mutates anything; the operator resolves
/// a real duplicate with `kb remember --supersedes` or `kb forget`.
///
/// **Data source, and why no new storage message was needed.** A true
/// cross-corpus nearest-neighbor route already exists
/// (`GET /api/kb/{kb}/atlas/similar/{id}`), but it's seed-one-doc /
/// single-corpus by construction (one `embedding_by_id` + one
/// `vector_query` against ONE table) — running it per-memory per-corpus to
/// build an all-pairs cross-corpus report would be O(n) round-trips per
/// corpus instead of one. Both memory corpora combined hold a few hundred
/// artifacts (confirmed against the live corpora — see the milestone
/// report), so a plain O(n^2) in-process comparison over each corpus's
/// FULL embedding set is cheap and exact. That full set is already a
/// standing read-lane call — `Storage::list_embeddings` (id, embedding)
/// pairs, corpus-wide, existing since W2.3a's true-neighbors work — joined
/// against a `list_docs` scan for title/forgotten/supersedes (the same two
/// calls `census` already makes). No new storage message, no new lance
/// projection: the cheapest correct path.
pub async fn dupes(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<DupesParams>,
) -> Response<Body> {
    let threshold = params
        .threshold
        .unwrap_or(kb_core::memory::DEFAULT_DUPES_THRESHOLD);
    if !(0.0..=1.0).contains(&threshold) {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "threshold must be in [0,1]; got {threshold}"
        )));
    }
    let limit = params
        .limit
        .unwrap_or(DUPES_DEFAULT_LIMIT)
        .min(DUPES_MAX_LIMIT);

    let restrict = match params.kb.as_deref() {
        Some(kb) => {
            let (name, ctx) = match crate::routes::resolve_kb(&state, kb) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            if ctx.memory_scope.is_none() {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "kb {name} is not a memory corpus"
                )));
            }
            Some(name)
        }
        None => None,
    };

    let corpora: Vec<(&KbName, &KbContext)> = state
        .kbs
        .iter()
        .filter(|(name, ctx)| {
            ctx.memory_scope.is_some() && restrict.as_ref().is_none_or(|r| r == *name)
        })
        .collect();

    struct DupesArm {
        docs: Vec<kb_core::storage::lance::DocSummary>,
        embeds: Vec<(String, Vec<f32>)>,
    }
    let mut futs: Vec<super::CorpusFut<'_, (&KbName, DupesArm)>> = Vec::new();
    for &(name, ctx) in &corpora {
        futs.push(Box::pin(async move {
            let (docs, embeds) = tokio::join!(
                ctx.storage.list_docs(u32::MAX),
                ctx.storage.list_embeddings(),
            );
            (
                name,
                DupesArm {
                    docs: docs.unwrap_or_default(),
                    embeds: embeds.unwrap_or_default(),
                },
            )
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let arms = super::buffered_join(futs, state.fanout_cap).await;

    let mut candidates: Vec<kb_core::memory::DupeCandidate> = Vec::new();
    for (name, arm) in arms {
        let embed_by_id: HashMap<String, Vec<f32>> = arm.embeds.into_iter().collect();
        for d in arm.docs {
            // MI-W4.0 — same predicate as census/recall; see
            // `is_recallable_memory_category`'s doc comment.
            if !kb_core::memory::is_recallable_memory_category(d.kb_category.as_deref()) {
                continue;
            }
            let embedding = embed_by_id.get(&d.id).cloned();
            candidates.push(kb_core::memory::DupeCandidate {
                kb: name.as_str().to_string(),
                id: d.id,
                title: d.title,
                forgotten: d.kb_status.as_deref() == Some("forgotten"),
                supersedes: d.kb_supersedes,
                embedding,
            });
        }
    }
    let scanned = candidates
        .iter()
        .filter(|c| !c.forgotten && c.embedding.is_some())
        .count() as u32;
    let pairs = kb_core::memory::find_duplicate_pairs(candidates, threshold, limit);

    Json(DupesResponse {
        pairs: pairs.into_iter().map(DupePairOut::from).collect(),
        threshold,
        scanned,
    })
    .into_response()
}

// === MI-W4.4 — hygiene triage queue ========================================

#[derive(Debug, Deserialize)]
pub struct TriageParams {
    /// Restrict to ONE memory corpus; absent ⇒ every memory corpus on the
    /// daemon (mirrors `DupesParams::kb`).
    pub kb: Option<String>,
    /// Bounded queue size — default `kb_core::triage::DEFAULT_QUEUE_SIZE`
    /// (~10, "an Anki-style ~10 items", not a full audit).
    pub limit: Option<usize>,
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryTriageItem")
)]
#[derive(Debug, Serialize)]
pub struct TriageItemOut {
    pub kb: String,
    pub id: String,
    pub title: String,
    /// Source-root-relative path — the SPA builds `/a/<kb>/<rel>` from it
    /// (mirrors `RecallResult::source_relative`). `None` in the vanishingly
    /// rare case the candidate's owning corpus vanished from `state.kbs`
    /// between the scan and the response being built (a concurrent kb
    /// removal) — the row still renders, just without a working link.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub source_relative: Option<String>,
    /// Stable machine tag: `below_floor_now` | `high_salience_dormant` |
    /// `duplicate` | `superseded_not_forgotten` | `flagged`.
    pub reason_kind: String,
    /// The one-line human justification, e.g. "salience 0.10 is at/below
    /// the 0.15 floor — excluded from recall now" — see
    /// `kb_core::triage::TriageReason::justification`.
    pub reason: String,
    pub urgency: f32,
    /// The ranking terms behind `reason` (mirroring `kb resurface
    /// --explain`'s style) — exactly the fields the chosen `reason_kind`
    /// populates; the rest are absent, never a re-derivable guess.
    /// Populated (alongside `salience`) for `below_floor_now` — the active
    /// policy's floor the salience is at/under RIGHT NOW, never a
    /// projected/future value (decay never lowers the value the floor
    /// tests; see `kb_core::memory::floor_state`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub floor: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub salience: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub dormant_days: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub duplicate_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub cosine: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub superseded_by: Option<String>,
    /// CT-C1 — populated for `flagged`: the flag's own (untruncated) reason
    /// text (`reason` above carries the excerpted justification line).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts-export", ts(optional))]
    pub flag_reason: Option<String>,
}

/// Not a `From` impl — `source_relative` isn't on `kb_core::triage::TriageItem`
/// at all (that crate has no concept of a source path; it's a display field
/// the ROUTE resolves), so the caller passes it in explicitly.
fn triage_item_out(
    item: kb_core::triage::TriageItem,
    source_relative: Option<String>,
) -> TriageItemOut {
    let reason_kind = item.reason.kind().to_string();
    let reason = item.reason.justification();
    let mut out = TriageItemOut {
        kb: item.kb,
        id: item.id,
        title: item.title,
        source_relative,
        reason_kind,
        reason,
        urgency: item.urgency,
        floor: None,
        salience: None,
        dormant_days: None,
        duplicate_of: None,
        cosine: None,
        superseded_by: None,
        flag_reason: None,
    };
    match item.reason {
        kb_core::triage::TriageReason::BelowFloorNow { salience, floor } => {
            out.salience = Some(salience);
            out.floor = Some(floor);
        }
        kb_core::triage::TriageReason::HighSalienceDormant {
            salience,
            dormant_days,
        } => {
            out.salience = Some(salience);
            out.dormant_days = dormant_days;
        }
        kb_core::triage::TriageReason::Duplicate { other_id, cosine } => {
            out.duplicate_of = Some(other_id);
            out.cosine = Some(cosine);
        }
        kb_core::triage::TriageReason::SupersededNotForgotten { superseded_by } => {
            out.superseded_by = Some(superseded_by);
        }
        kb_core::triage::TriageReason::Flagged { reason } => {
            out.flag_reason = Some(reason);
        }
    }
    out
}

#[cfg_attr(
    feature = "ts-export",
    derive(ts_rs::TS),
    ts(export, rename = "MemoryTriageResponse")
)]
#[derive(Debug, Serialize)]
pub struct TriageResponse {
    pub items: Vec<TriageItemOut>,
    /// Total memory artifacts considered (post category-eligibility filter,
    /// pre pinned/forgotten/no-reason exclusion) — NOT `items.len()`, so a
    /// caller can tell "10 items because the corpus is huge" from "10 items
    /// because that's everything".
    pub scanned: u32,
    /// FIX2 (kb-core/triage.rs duplication risk) — `kb_core::triage::
    /// HIGH_SALIENCE_THRESHOLD`, wire-supplied so the SPA's salience ×
    /// recall quadrant scatter (`lib/recallQuadrant.ts`, whose "dead
    /// weight"/"mis-scored" quadrants must agree with THIS route's
    /// `high_salience_dormant` reason on what counts as high salience)
    /// reads the SAME constant instead of hand-duplicating it — the same
    /// drift risk `decay_k`/`drop_threshold` already avoid on other routes.
    pub high_salience_threshold: f32,
    /// `kb_core::triage::DORMANT_DAYS` — same reasoning.
    pub dormant_days: f32,
}

/// CT-C1 — bounded sync scan for the OPEN `[kb-flag]` comment on every
/// artifact under `dir` (one kb's `.review/` directory). Mirrors
/// `list_reviews`'s own directory-walk technique: bounded to however many
/// artifacts in THIS kb actually have a review file at all — never the full
/// memory corpus, and never one `.review/<id>.json` read per candidate
/// memory. `dir` not existing yet (no comments anywhere in this kb) is the
/// common case, not an error. Returns `(artifact_id -> reason text)`, one
/// entry per flagged artifact — its OLDEST open flag comment, ties broken
/// by comment id, so the result is deterministic across identical scans.
/// Non-memory artifacts with a flag comment are harmlessly present in the
/// map too; the caller only ever looks up ids that are ALSO memory
/// candidates.
fn flagged_reasons_in_dir(dir: &std::path::Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(Some(file)) = kb_core::review::load(&path) else {
            continue;
        };
        let mut best: Option<&kb_core::review::Comment> = None;
        for c in &file.comments {
            if c.status != kb_core::review::CommentStatus::Open {
                continue;
            }
            if !kb_core::memory::is_flag_comment(&c.body) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => (c.created_at, &c.id) < (b.created_at, &b.id),
            };
            if better {
                best = Some(c);
            }
        }
        if let Some(reason) = best.and_then(|c| kb_core::memory::flag_reason(&c.body)) {
            out.insert(stem.to_string(), reason.to_string());
        }
    }
    out
}

/// Async wrapper running [`flagged_reasons_in_dir`] off the tokio worker
/// (a directory walk + per-file JSON parse is sync IO); a `spawn_blocking`
/// panic/cancellation degrades to "nothing flagged" rather than failing the
/// whole triage scan for one corpus (invariant #28's posture).
async fn scan_flagged_reasons(dir: std::path::PathBuf) -> HashMap<String, String> {
    tokio::task::spawn_blocking(move || flagged_reasons_in_dir(&dir))
        .await
        .unwrap_or_default()
}

/// `GET /api/memory/triage[?kb=][&limit=]` — MI-W4.4: the bounded, DERIVED
/// hygiene queue (`kb memory triage`'s data source, and the SPA's matching
/// panel). Gathers candidates from the SAME reads census/dupes/lineage
/// already use (never a new store): `list_docs` + `pinned_memories_set`
/// per corpus (invariant #28 fan-out), the corpus-local reverse
/// `kb-supersedes` map (census's own technique), the MI-W3.1 duplicate scan
/// (`find_duplicate_pairs` at its own default 0.90 threshold — this route
/// doesn't expose `--threshold`; `kb memory dupes` is the tool for tuning
/// that), and the recall-usage ledger fan-out (`fetch_recall_stats`, shared
/// with `recall`/`census`). Scores with `kb_core::triage::build_queue` and
/// returns AT MOST `limit` items. Never mutates anything.
pub async fn triage(
    State(state): State<Arc<KbHandles>>,
    Query(params): Query<TriageParams>,
) -> Response<Body> {
    let limit = params
        .limit
        .unwrap_or(kb_core::triage::DEFAULT_QUEUE_SIZE)
        .clamp(1, 100);

    let restrict = match params.kb.as_deref() {
        Some(kb) => {
            let (name, ctx) = match crate::routes::resolve_kb(&state, kb) {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            if ctx.memory_scope.is_none() {
                return error_to_problem_json(&kb_core::Error::BadRequest(format!(
                    "kb {name} is not a memory corpus"
                )));
            }
            Some(name)
        }
        None => None,
    };
    let corpora: Vec<(&KbName, &KbContext)> = state
        .kbs
        .iter()
        .filter(|(name, ctx)| {
            ctx.memory_scope.is_some() && restrict.as_ref().is_none_or(|r| r == *name)
        })
        .collect();

    let daemon_policy = *state.memory_policy.read().await;

    struct TriageArm {
        docs: Vec<kb_core::storage::lance::DocSummary>,
        embeds: Vec<(String, Vec<f32>)>,
        pinned: HashSet<String>,
        floor: Option<f32>,
        source_path: std::path::PathBuf,
        /// CT-C1 — `(artifact_id -> flag reason)`, from a bounded `.review/`
        /// directory scan (see `flagged_reasons_in_dir`), NOT one file read
        /// per candidate memory.
        flagged: HashMap<String, String>,
    }
    let paths = state.paths.clone();
    let mut futs: Vec<super::CorpusFut<'_, (&KbName, TriageArm)>> = Vec::new();
    for &(name, ctx) in &corpora {
        let review_dir = paths.kb_review_dir(name);
        futs.push(Box::pin(async move {
            let (docs, embeds, pinned, flagged) = tokio::join!(
                ctx.storage.list_docs(u32::MAX),
                ctx.storage.list_embeddings(),
                ctx.storage.pinned_memories_set(),
                scan_flagged_reasons(review_dir),
            );
            let policy = ctx.memory_decay_policy.unwrap_or(daemon_policy);
            let threshold = policy.drop_threshold();
            (
                name,
                TriageArm {
                    docs: docs.unwrap_or_default(),
                    embeds: embeds.unwrap_or_default(),
                    pinned: pinned.unwrap_or_default(),
                    floor: threshold.is_finite().then_some(threshold),
                    source_path: ctx.source_path.clone(),
                    flagged,
                },
            )
        }));
    }
    // PF-R1 — the operator-configurable `[server] fanout_cap` (default 8,
    // byte-identical to the old hardcoded `super::FANOUT_CAP`).
    let arms = super::buffered_join(futs, state.fanout_cap).await;

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Fold — per-kb filtered memory rows (MI-W4.0 predicate), pinned sets,
    // per-corpus floors, and the dupe-candidate list (unfiltered docs feed
    // it; `find_duplicate_pairs` applies its own forgotten/embedding gate).
    let mut floor_by_kb: HashMap<String, Option<f32>> = HashMap::new();
    let mut pinned_by_kb: HashMap<String, HashSet<String>> = HashMap::new();
    let mut source_path_by_kb: HashMap<String, std::path::PathBuf> = HashMap::new();
    let mut memories_by_kb: HashMap<String, Vec<kb_core::storage::lance::DocSummary>> =
        HashMap::new();
    let mut flagged_by_kb: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut dupe_candidates: Vec<kb_core::memory::DupeCandidate> = Vec::new();
    for (name, arm) in arms {
        let kb_str = name.as_str().to_string();
        floor_by_kb.insert(kb_str.clone(), arm.floor);
        pinned_by_kb.insert(kb_str.clone(), arm.pinned);
        source_path_by_kb.insert(kb_str.clone(), arm.source_path);
        flagged_by_kb.insert(kb_str.clone(), arm.flagged);
        let embed_by_id: HashMap<String, Vec<f32>> = arm.embeds.into_iter().collect();
        let mut kept = Vec::new();
        for d in arm.docs {
            if !kb_core::memory::is_recallable_memory_category(d.kb_category.as_deref()) {
                continue;
            }
            let embedding = embed_by_id.get(&d.id).cloned();
            dupe_candidates.push(kb_core::memory::DupeCandidate {
                kb: kb_str.clone(),
                id: d.id.clone(),
                title: d.title.clone(),
                forgotten: d.kb_status.as_deref() == Some("forgotten"),
                supersedes: d.kb_supersedes.clone(),
                embedding,
            });
            kept.push(d);
        }
        memories_by_kb.insert(kb_str, kept);
    }

    // Reverse `kb-supersedes` map, corpus-local (mirrors `census`'s own
    // `superseded_by` computation) — keyed by (kb, target-id).
    let mut superseded_by: HashMap<(String, String), String> = HashMap::new();
    for (kb_str, docs) in &memories_by_kb {
        for d in docs {
            if let Some(target) = &d.kb_supersedes {
                superseded_by
                    .entry((kb_str.clone(), target.clone()))
                    .or_insert_with(|| d.id.clone());
            }
        }
    }

    // Duplicate pairs at the report's own default threshold — one flagged
    // partner per candidate (the pair's OTHER member + cosine), keeping the
    // HIGHEST-cosine pairing when a memory shows up in more than one pair.
    let pairs = kb_core::memory::find_duplicate_pairs(
        dupe_candidates,
        kb_core::memory::DEFAULT_DUPES_THRESHOLD,
        usize::MAX,
    );
    let mut dupe_of: HashMap<(String, String), (String, f32)> = HashMap::new();
    for p in &pairs {
        dupe_of
            .entry((p.kb_a.clone(), p.id_a.clone()))
            .and_modify(|cur: &mut (String, f32)| {
                if p.cosine > cur.1 {
                    *cur = (p.id_b.clone(), p.cosine);
                }
            })
            .or_insert_with(|| (p.id_b.clone(), p.cosine));
        dupe_of
            .entry((p.kb_b.clone(), p.id_b.clone()))
            .and_modify(|cur: &mut (String, f32)| {
                if p.cosine > cur.1 {
                    *cur = (p.id_a.clone(), p.cosine);
                }
            })
            .or_insert_with(|| (p.id_a.clone(), p.cosine));
    }

    // Recall-usage stats fan-out — same helper `recall`/`census` use.
    let mut ids_by_kb: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for (kb_str, docs) in &memories_by_kb {
        ids_by_kb.insert(kb_str.clone(), docs.iter().map(|d| d.id.clone()).collect());
    }
    let recall_stats = fetch_recall_stats(&state, &ids_by_kb).await;

    let mut candidates: Vec<kb_core::triage::TriageCandidate> = Vec::new();
    let mut source_relative_by_id: HashMap<(String, String), String> = HashMap::new();
    for (kb_str, docs) in &memories_by_kb {
        let pinned = pinned_by_kb.get(kb_str);
        for d in docs {
            if let Some(sp) = source_path_by_kb.get(kb_str) {
                source_relative_by_id.insert(
                    (kb_str.clone(), d.id.clone()),
                    kb_core::paths::doc_rel_path(&d.path, sp),
                );
            }
            // CT-C5 — `used_count` (the 3rd slot) is unused here: the
            // hygiene/triage queue doesn't surface it (out of scope; see
            // `TriageCandidate`'s field list).
            let (recall_count, last_recalled_at, _used_count) = recall_stats
                .get(&(kb_str.clone(), d.id.clone()))
                .cloned()
                .unwrap_or((0, None, 0));
            candidates.push(kb_core::triage::TriageCandidate {
                kb: kb_str.clone(),
                id: d.id.clone(),
                title: d.title.clone(),
                pinned: pinned.map(|s| s.contains(&d.id)).unwrap_or(false),
                forgotten: d.kb_status.as_deref() == Some("forgotten"),
                salience: d.kb_salience,
                recall_count,
                last_recalled_at,
                superseded_by: superseded_by.get(&(kb_str.clone(), d.id.clone())).cloned(),
                dupe_of: dupe_of.get(&(kb_str.clone(), d.id.clone())).cloned(),
                flagged_reason: flagged_by_kb
                    .get(kb_str)
                    .and_then(|m| m.get(&d.id).cloned()),
            });
        }
    }

    let scanned = candidates.len() as u32;
    let items = kb_core::triage::build_queue(&candidates, now_unix, &floor_by_kb, limit);
    let items_out: Vec<TriageItemOut> = items
        .into_iter()
        .map(|it| {
            let source_relative = source_relative_by_id
                .get(&(it.kb.clone(), it.id.clone()))
                .cloned();
            triage_item_out(it, source_relative)
        })
        .collect();
    Json(TriageResponse {
        items: items_out,
        scanned,
        high_salience_threshold: kb_core::triage::HIGH_SALIENCE_THRESHOLD,
        dormant_days: kb_core::triage::DORMANT_DAYS,
    })
    .into_response()
}

/// PUT /api/memory/policy — flip the daemon-wide decay policy.
/// v0.12 persists to `<state>/memory-policy.json` so the choice
/// survives daemon restarts. Per-kb override in kb.toml stays a
/// v0.13 task.
pub async fn put_policy(
    State(state): State<Arc<KbHandles>>,
    Json(body): Json<PolicyBody>,
) -> Response<Body> {
    let Some(next) = DecayPolicy::parse(&body.policy) else {
        return error_to_problem_json(&kb_core::Error::BadRequest(format!(
            "policy must be one of strict|balanced|loose; got {:?}",
            body.policy
        )));
    };
    *state.memory_policy.write().await = next;
    if let Err(e) = crate::state::save_memory_policy(&state.paths, next) {
        // Persistence failure shouldn't fail the request — the daemon
        // cell is the source of truth at runtime — but log loudly so
        // the operator notices the restart-resilience is broken.
        tracing::warn!(error = %e, "failed to persist memory-policy.json");
    }
    Json(PolicyBody {
        policy: next.as_str().to_string(),
        drop_threshold: wire_drop_threshold(next),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(kb: &str, id: &str) -> RecallResult {
        RecallResult {
            id: id.to_string(),
            kb: kb.to_string(),
            title: id.to_string(),
            path: format!("/{id}.html"),
            source_relative: format!("{id}.html"),
            score: 1.0,
            salience: 0.5,
            pinned: false,
            session_id: None,
            summary: None,
            memory_type: None,
            source: None,
            author: None,
            source_kb: None,
            source_artifact: None,
            source_anchor: None,
            global: false,
            linked_kbs: Vec::new(),
            read_pct: None,
            last_read_at: None,
            stopped_at: None,
            rank: None,
            rel: None,
            decay: None,
            age_days: None,
            recall_count: 0,
            last_recalled_at: None,
            recall_used_count: 0,
            relevance_factor: None,
            stability: None,
            decay_k: None,
            recall_weekly: Vec::new(),
            flagged: false,
            warns: false,
            code_hints: Vec::new(),
            code_hints_total: 0,
            drift_open: 0,
        }
    }

    /// MI-W1.3 — `apply_recall_stats` must NEVER reorder, drop, or add
    /// hits: it only ever writes `recall_count`/`last_recalled_at` onto the
    /// existing slice, in place, by `(kb, id)` lookup. The rerank ordering
    /// contract (score-descending) is established upstream and must survive
    /// this enrichment pass byte-for-byte.
    #[test]
    fn apply_recall_stats_never_reorders_or_drops_hits() {
        let mut hits = vec![
            hit("notes", "aaaaaaaaaaaa"),
            hit("notes", "bbbbbbbbbbbb"),
            hit("other", "aaaaaaaaaaaa"),
        ];
        let ids_before: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();

        let mut stats: HashMap<(String, String), (u32, Option<i64>, u32)> = HashMap::new();
        stats.insert(("notes".into(), "bbbbbbbbbbbb".into()), (3, Some(500), 2));
        // A stats entry for an id/kb pair that ISN'T in `hits` at all must
        // be silently ignored, never inserted as a new row.
        stats.insert(("nowhere".into(), "cccccccccccc".into()), (99, Some(1), 99));

        apply_recall_stats(&mut hits, &stats);

        let ids_after: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        assert_eq!(ids_before, ids_after, "order + membership unchanged");
        assert_eq!(hits.len(), 3, "no rows added or dropped");

        // Only the matched hit was touched.
        assert_eq!(hits[0].recall_count, 0);
        assert!(hits[0].last_recalled_at.is_none());
        assert_eq!(hits[1].recall_count, 3);
        assert_eq!(hits[1].last_recalled_at, Some(500));
        assert_eq!(hits[1].recall_used_count, 2);
        // Same memory id but a DIFFERENT kb — must NOT pick up notes'
        // stats (the (kb, id) pair, not id alone, is the key).
        assert_eq!(hits[2].recall_count, 0);
        assert!(hits[2].last_recalled_at.is_none());
        assert_eq!(hits[2].recall_used_count, 0);
    }

    /// A hit with no matching ledger row keeps the zero/absent defaults —
    /// `apply_recall_stats` never invents a count.
    #[test]
    fn apply_recall_stats_leaves_unmatched_hits_at_defaults() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa")];
        apply_recall_stats(&mut hits, &HashMap::new());
        assert_eq!(hits[0].recall_count, 0);
        assert!(hits[0].last_recalled_at.is_none());
    }

    // CT-A1 (U3 parse-back) — `parse_source_anchor` is the one new pure fn
    // this route module adds; the fan-out handler (`memories_from`) and the
    // RecallResult/CensusRow field wiring are exercised by kb-core's storage
    // round-trip tests (`storage/lance.rs`) + the full indexer-path test
    // (`indexer.rs::u3_provenance_survives_the_full_indexer_path`).
    #[test]
    fn parse_source_anchor_round_trips_valid_json() {
        let json = kb_core::lists::anchor_to_json(&kb_core::review::Anchor::Section {
            id: "intro".into(),
            tag: None,
            snippet: None,
        });
        let parsed = parse_source_anchor(Some(&json)).expect("valid JSON must parse");
        assert_eq!(
            parsed,
            kb_core::review::Anchor::Section {
                id: "intro".into(),
                tag: None,
                snippet: None,
            }
        );
    }

    #[test]
    fn parse_source_anchor_is_none_for_absent_or_malformed_input() {
        assert!(parse_source_anchor(None).is_none());
        assert!(parse_source_anchor(Some("")).is_none());
        assert!(parse_source_anchor(Some("not json")).is_none());
        // A well-formed JSON object that just isn't a valid Anchor variant.
        assert!(parse_source_anchor(Some(r#"{"kind":"not-a-real-anchor"}"#)).is_none());
    }

    // --- CT-C1 — the `flagged` post-rank enrichment ------------------------

    /// `apply_flagged` must NEVER reorder, drop, or add hits — same
    /// contract as `apply_recall_stats`.
    #[test]
    fn apply_flagged_never_reorders_or_drops_hits() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        let ids_before: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        let mut flagged = HashSet::new();
        flagged.insert(("notes".to_string(), "bbbbbbbbbbbb".to_string()));

        apply_flagged(&mut hits, &flagged);

        let ids_after: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        assert_eq!(ids_before, ids_after);
        assert!(!hits[0].flagged);
        assert!(hits[1].flagged);
    }

    /// SURFACED, NEVER SCORED (invariant #10's posture, extended to CT-C1):
    /// a flagged and an unflagged hit at IDENTICAL score/rank/rel/decay must
    /// stay identical after `apply_flagged` — flagging one can only ever
    /// touch its own `.flagged` bool.
    #[test]
    fn apply_flagged_never_touches_the_scoring_decomposition() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        for h in &mut hits {
            h.score = 0.42;
            h.rank = Some(3);
            h.rel = Some(0.1);
            h.decay = Some(0.9);
            h.age_days = Some(5.0);
        }
        let mut flagged = HashSet::new();
        flagged.insert(("notes".to_string(), "aaaaaaaaaaaa".to_string()));

        apply_flagged(&mut hits, &flagged);

        assert!(hits[0].flagged);
        assert!(!hits[1].flagged);
        assert_eq!(hits[0].score, hits[1].score, "flagging must not tilt score");
        assert_eq!(hits[0].rank, hits[1].rank);
        assert_eq!(hits[0].rel, hits[1].rel);
        assert_eq!(hits[0].decay, hits[1].decay);
        assert_eq!(hits[0].age_days, hits[1].age_days);
    }

    // --- CT-C3 — the `warns` (failed-outcome) post-rank enrichment ---------

    /// `apply_warns` must NEVER reorder, drop, or add hits — same contract
    /// as `apply_flagged`/`apply_recall_stats`.
    #[test]
    fn apply_warns_never_reorders_or_drops_hits() {
        let mut hits = vec![
            hit("notes", "aaaaaaaaaaaa"),
            hit("notes", "bbbbbbbbbbbb"),
            hit("other", "aaaaaaaaaaaa"),
        ];
        let ids_before: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        let mut failed = HashSet::new();
        failed.insert(("notes".to_string(), "bbbbbbbbbbbb".to_string()));

        apply_warns(&mut hits, &failed);

        let ids_after: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        assert_eq!(ids_before, ids_after);
        assert!(!hits[0].warns);
        assert!(hits[1].warns);
        // Keyed by (kb, id) — the SAME id in a different kb is untouched.
        assert!(!hits[2].warns);
    }

    /// SURFACED, NEVER SCORED (the twin of `apply_flagged_never_touches_
    /// the_scoring_decomposition`): a failed-outcome and an ordinary hit at
    /// IDENTICAL score/rank/rel/decay must stay identical after
    /// `apply_warns` — the marker can only ever touch its own `.warns`
    /// bool, never the decomposition `rerank_with_policy_scored` fixed.
    #[test]
    fn apply_warns_never_touches_the_scoring_decomposition() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        for h in &mut hits {
            h.score = 0.42;
            h.rank = Some(3);
            h.rel = Some(0.1);
            h.decay = Some(0.9);
            h.age_days = Some(5.0);
        }
        let mut failed = HashSet::new();
        failed.insert(("notes".to_string(), "aaaaaaaaaaaa".to_string()));

        apply_warns(&mut hits, &failed);

        assert!(hits[0].warns);
        assert!(!hits[1].warns);
        assert_eq!(
            hits[0].score, hits[1].score,
            "a failed outcome must not tilt score"
        );
        assert_eq!(hits[0].rank, hits[1].rank);
        assert_eq!(hits[0].rel, hits[1].rel);
        assert_eq!(hits[0].decay, hits[1].decay);
        assert_eq!(hits[0].age_days, hits[1].age_days);
        // `warns` and `flagged` are independent fields — marking one never
        // bleeds into the other (the hook composes them, the wire doesn't).
        assert!(!hits[0].flagged && !hits[1].flagged);
    }

    /// The failed set is derived from `DocSummary.tags` through the ONE
    /// kb-core grammar — the wire stays absent-when-false so the hook's
    /// pre-CT-C3 rendering of ordinary hits is byte-identical.
    #[test]
    fn warns_serializes_absent_when_false_and_true_when_set() {
        let mut h = hit("notes", "aaaaaaaaaaaa");
        let plain = serde_json::to_value(&h).expect("serialize");
        assert!(
            plain.get("warns").is_none(),
            "warns must be ABSENT (not false) on an ordinary hit"
        );
        h.warns = true;
        let warned = serde_json::to_value(&h).expect("serialize");
        assert_eq!(warned.get("warns"), Some(&serde_json::Value::Bool(true)));
    }

    fn test_paths(root: &std::path::Path) -> kb_core::paths::KbPaths {
        kb_core::paths::KbPaths {
            state: root.join("state"),
            config: root.join("config"),
            cache: root.join("cache"),
            log: root.join("log"),
            runs: root.join("runs"),
            quarantine: root.join("quarantine"),
            exports: root.join("exports"),
            daemon_name: "test".to_string(),
        }
    }

    fn write_review_with_comment(
        paths: &kb_core::paths::KbPaths,
        kb: &str,
        id: &str,
        body: &str,
        status: kb_core::review::CommentStatus,
    ) {
        let kb_name = KbName::new(kb).expect("valid kb name");
        let path = paths.kb_review_file(&kb_name, id);
        std::fs::create_dir_all(path.parent().expect("has parent")).expect("mkdir .review");
        let mut file = kb_core::review::ReviewFile::empty_skeleton(&kb_name, id, "title");
        let cid = file
            .add_comment(kb_core::review::NewComment {
                file: id.to_string(),
                file_label: "main".to_string(),
                anchor: kb_core::review::Anchor::File,
                author: kb_core::review::Author::Claude,
                body: body.to_string(),
                choices: Vec::new(),
                attachments: Vec::new(),
                user: None,
            })
            .id
            .clone();
        if status == kb_core::review::CommentStatus::Resolved {
            file.set_comment_status(&cid, status)
                .expect("comment exists");
        }
        kb_core::review::save_atomic(&path, &file, None).expect("save review fixture");
    }

    /// `fetch_review_marks` against REAL `.review/<id>.json` fixtures: an
    /// OPEN `[kb-flag]` comment counts, a RESOLVED one doesn't, an ordinary
    /// (non-flag) open comment doesn't, and a hit with no review file at
    /// all is simply absent — never an error, never a guess. (The CT-C1
    /// flag behaviour, unchanged through the CT-C4 one-pass refactor.)
    #[test]
    fn fetch_review_marks_reads_real_review_files_bounded_to_the_returned_hits() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(tmp.path());
        write_review_with_comment(
            &paths,
            "notes",
            "aaaaaaaaaaaa",
            "[kb-flag] this contradicts the newer memory",
            kb_core::review::CommentStatus::Open,
        );
        write_review_with_comment(
            &paths,
            "notes",
            "bbbbbbbbbbbb",
            "[kb-flag] already handled",
            kb_core::review::CommentStatus::Resolved,
        );
        write_review_with_comment(
            &paths,
            "notes",
            "cccccccccccc",
            "just an ordinary comment, not a flag",
            kb_core::review::CommentStatus::Open,
        );
        let hits = vec![
            hit("notes", "aaaaaaaaaaaa"),
            hit("notes", "bbbbbbbbbbbb"),
            hit("notes", "cccccccccccc"),
            hit("notes", "dddddddddddd"), // no review file at all
        ];

        let marks = fetch_review_marks(&paths, &hits);

        assert_eq!(
            marks.flagged,
            HashSet::from([("notes".to_string(), "aaaaaaaaaaaa".to_string())])
        );
        assert!(
            marks.drift_open.is_empty(),
            "no [kb-drift] comments anywhere — no drift entries"
        );
    }

    // --- CT-C4 — drift marks from the SAME single review pass --------------

    /// One review file carrying an open flag AND two open drift comments
    /// (plus a resolved drift + an ordinary comment as decoys) yields BOTH
    /// marks from the one `fetch_review_marks` call — the single-read-pass
    /// contract: there is no second flag/drift reader on the recall path,
    /// so one `review::load` per hit serves `flagged` and `drift_open`.
    #[test]
    fn fetch_review_marks_returns_flag_and_drift_from_one_pass() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(tmp.path());
        let kb_name = KbName::new("notes").expect("valid kb name");
        let path = paths.kb_review_file(&kb_name, "aaaaaaaaaaaa");
        std::fs::create_dir_all(path.parent().expect("has parent")).expect("mkdir .review");
        let mut file = kb_core::review::ReviewFile::empty_skeleton(&kb_name, "aaaaaaaaaaaa", "t");
        let mut add = |body: &str, status: kb_core::review::CommentStatus| {
            let cid = file
                .add_comment(kb_core::review::NewComment {
                    file: "aaaaaaaaaaaa".to_string(),
                    file_label: "main".to_string(),
                    anchor: kb_core::review::Anchor::File,
                    author: kb_core::review::Author::Claude,
                    body: body.to_string(),
                    choices: Vec::new(),
                    attachments: Vec::new(),
                    user: None,
                })
                .id
                .clone();
            if status == kb_core::review::CommentStatus::Resolved {
                file.set_comment_status(&cid, status)
                    .expect("comment exists");
            }
        };
        add(
            "[kb-flag] the fact itself is disputed",
            kb_core::review::CommentStatus::Open,
        );
        add(
            "[kb-drift] src/storage/actor.rs:1502-1516 — absent in kb@abc1234 (swept 2026-08-21)",
            kb_core::review::CommentStatus::Open,
        );
        add(
            "[kb-drift] crates/kb-core/src/memory.rs — moved",
            kb_core::review::CommentStatus::Open,
        );
        add(
            "[kb-drift] src/gone.rs — re-verified, resolved",
            kb_core::review::CommentStatus::Resolved,
        );
        add(
            "an ordinary open comment",
            kb_core::review::CommentStatus::Open,
        );
        kb_core::review::save_atomic(&path, &file, None).expect("save review fixture");

        // A drift-only sibling: drift never implies flagged.
        write_review_with_comment(
            &paths,
            "notes",
            "bbbbbbbbbbbb",
            "[kb-drift] web/src/api.ts — rotted",
            kb_core::review::CommentStatus::Open,
        );

        let hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        let marks = fetch_review_marks(&paths, &hits);

        assert_eq!(
            marks.flagged,
            HashSet::from([("notes".to_string(), "aaaaaaaaaaaa".to_string())])
        );
        assert_eq!(
            marks.drift_open,
            HashMap::from([
                (("notes".to_string(), "aaaaaaaaaaaa".to_string()), 2),
                (("notes".to_string(), "bbbbbbbbbbbb".to_string()), 1),
            ]),
            "open drift comments counted; resolved + ordinary ones ignored"
        );
    }

    /// `apply_drift_open` must NEVER reorder, drop, or add hits — same
    /// contract as `apply_flagged`/`apply_recall_stats`.
    #[test]
    fn apply_drift_open_never_reorders_or_drops_hits() {
        let mut hits = vec![
            hit("notes", "aaaaaaaaaaaa"),
            hit("notes", "bbbbbbbbbbbb"),
            hit("other", "aaaaaaaaaaaa"),
        ];
        let ids_before: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        let mut drift = HashMap::new();
        drift.insert(("notes".to_string(), "bbbbbbbbbbbb".to_string()), 2u32);

        apply_drift_open(&mut hits, &drift);

        let ids_after: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        assert_eq!(ids_before, ids_after);
        assert_eq!(hits[0].drift_open, 0);
        assert_eq!(hits[1].drift_open, 2);
        // Keyed by (kb, id) — the SAME id in a different kb is untouched.
        assert_eq!(hits[2].drift_open, 0);
    }

    /// SURFACED, NEVER SCORED (the twin of `apply_flagged_never_touches_
    /// the_scoring_decomposition`): a drift-marked and an ordinary hit at
    /// IDENTICAL score/rank/rel/decay must stay identical after
    /// `apply_drift_open` — the count can only ever touch its own
    /// `.drift_open`, never the decomposition rerank fixed.
    #[test]
    fn apply_drift_open_never_touches_the_scoring_decomposition() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        for h in &mut hits {
            h.score = 0.42;
            h.rank = Some(3);
            h.rel = Some(0.1);
            h.decay = Some(0.9);
            h.age_days = Some(5.0);
        }
        let mut drift = HashMap::new();
        drift.insert(("notes".to_string(), "aaaaaaaaaaaa".to_string()), 3u32);

        apply_drift_open(&mut hits, &drift);

        assert_eq!(hits[0].drift_open, 3);
        assert_eq!(hits[1].drift_open, 0);
        assert_eq!(
            hits[0].score, hits[1].score,
            "an open drift comment must not tilt score"
        );
        assert_eq!(hits[0].rank, hits[1].rank);
        assert_eq!(hits[0].rel, hits[1].rel);
        assert_eq!(hits[0].decay, hits[1].decay);
        assert_eq!(hits[0].age_days, hits[1].age_days);
        // drift_open, flagged and warns are independent fields — marking
        // drift never bleeds into the other two.
        assert!(!hits[0].flagged && !hits[0].warns);
    }

    // --- CT-C4 — the code_hints post-rank enrichment -----------------------

    fn code_ref_row(ordinal: u32, kind: &str, path_hint: Option<&str>) -> CodeRefRow {
        CodeRefRow {
            ordinal,
            kind: kind.to_string(),
            raw_text: path_hint.unwrap_or("raw").to_string(),
            path_hint: path_hint.map(str::to_string),
            line_start: None,
            line_end: None,
            line_spans: None,
            symbol_container: None,
            symbol_member: None,
            context: String::new(),
            context_tokens: String::new(),
            group_key: None,
            group_label: None,
            group_anchor: None,
            declared: false,
        }
    }

    /// `code_hint_paths` keeps document order, dedups on the path itself
    /// (two line-cites of one file are ONE hint), and only path-shaped
    /// kinds contribute — `issue` (org/repo slug), `external` (gem/vendor)
    /// and symbol kinds are never scent.
    #[test]
    fn code_hint_paths_filters_to_path_kinds_and_dedups_in_document_order() {
        let refs = vec![
            code_ref_row(0, "path", Some("src/b.rs")),
            code_ref_row(1, "path_line", Some("src/a.rs")),
            // Same file cited again with a range — dedup, not a new hint.
            code_ref_row(2, "path_range", Some("src/a.rs")),
            code_ref_row(3, "issue", Some("owner/repo")),
            code_ref_row(4, "external", Some("node_modules/x/y.js")),
            code_ref_row(5, "symbol_method", None),
            code_ref_row(6, "path_list", Some("web/src/api.ts")),
            // A path row with no hint recorded contributes nothing.
            code_ref_row(7, "path", None),
        ];
        let (paths, total) = code_hint_paths(&refs);
        assert_eq!(paths, vec!["src/b.rs", "src/a.rs", "web/src/api.ts"]);
        assert_eq!(total, 3, "total counts distinct FILES, not citations");
    }

    /// Truncation is explicit, never silent: 7 distinct files cap to
    /// `CODE_HINTS_CAP` (=5) on the wire list while `total` still says 7.
    #[test]
    fn code_hint_paths_caps_the_list_but_reports_the_full_total() {
        let files = [
            "src/a.rs", "src/b.rs", "src/c.rs", "src/d.rs", "src/e.rs", "src/f.rs", "src/g.rs",
        ];
        let refs: Vec<CodeRefRow> = files
            .iter()
            .enumerate()
            .map(|(i, f)| code_ref_row(i as u32, "path", Some(f)))
            .collect();
        let (paths, total) = code_hint_paths(&refs);
        assert_eq!(paths.len(), CODE_HINTS_CAP);
        assert_eq!(paths, files[..CODE_HINTS_CAP].to_vec());
        assert_eq!(total, 7, "the pre-cap distinct count survives the cap");

        // And the empty case is honestly (empty, 0) — the fetch pass skips
        // inserting such an entry at all.
        let (empty_paths, empty_total) = code_hint_paths(&[]);
        assert!(empty_paths.is_empty());
        assert_eq!(empty_total, 0);
    }

    /// `apply_code_hints` must NEVER reorder, drop, or add hits — same
    /// contract as its sibling passes.
    #[test]
    fn apply_code_hints_never_reorders_or_drops_hits() {
        let mut hits = vec![
            hit("notes", "aaaaaaaaaaaa"),
            hit("notes", "bbbbbbbbbbbb"),
            hit("other", "aaaaaaaaaaaa"),
        ];
        let ids_before: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        let mut hints = HashMap::new();
        hints.insert(
            ("notes".to_string(), "bbbbbbbbbbbb".to_string()),
            (vec!["src/a.rs".to_string()], 1u32),
        );

        apply_code_hints(&mut hits, &hints);

        let ids_after: Vec<(String, String)> =
            hits.iter().map(|h| (h.kb.clone(), h.id.clone())).collect();
        assert_eq!(ids_before, ids_after);
        assert!(hits[0].code_hints.is_empty());
        assert_eq!(hits[0].code_hints_total, 0);
        assert_eq!(hits[1].code_hints, vec!["src/a.rs".to_string()]);
        assert_eq!(hits[1].code_hints_total, 1);
        // Keyed by (kb, id) — the SAME id in a different kb is untouched.
        assert!(hits[2].code_hints.is_empty());
    }

    /// SURFACED, NEVER SCORED (the twin of the flagged/warns/drift
    /// isolation tests): a hint-carrying and a hint-less hit at IDENTICAL
    /// score/rank/rel/decay must stay identical after `apply_code_hints` —
    /// the hints can only ever touch `.code_hints`/`.code_hints_total`.
    #[test]
    fn apply_code_hints_never_touches_the_scoring_decomposition() {
        let mut hits = vec![hit("notes", "aaaaaaaaaaaa"), hit("notes", "bbbbbbbbbbbb")];
        for h in &mut hits {
            h.score = 0.42;
            h.rank = Some(3);
            h.rel = Some(0.1);
            h.decay = Some(0.9);
            h.age_days = Some(5.0);
        }
        let mut hints = HashMap::new();
        hints.insert(
            ("notes".to_string(), "aaaaaaaaaaaa".to_string()),
            (vec!["src/a.rs".to_string(), "src/b.rs".to_string()], 7u32),
        );

        apply_code_hints(&mut hits, &hints);

        assert_eq!(hits[0].code_hints.len(), 2);
        assert_eq!(hits[0].code_hints_total, 7);
        assert!(hits[1].code_hints.is_empty());
        assert_eq!(
            hits[0].score, hits[1].score,
            "code hints must not tilt score"
        );
        assert_eq!(hits[0].rank, hits[1].rank);
        assert_eq!(hits[0].rel, hits[1].rel);
        assert_eq!(hits[0].decay, hits[1].decay);
        assert_eq!(hits[0].age_days, hits[1].age_days);
    }

    /// The CT-C4 wire contract: all three fields ABSENT on an ordinary hit
    /// (so the hook's pre-CT-C4 rendering is byte-identical), present when
    /// set — the u32s via `is_zero_u32`, the Vec via `Vec::is_empty`.
    #[test]
    fn code_hints_and_drift_open_serialize_absent_when_empty_or_zero() {
        let mut h = hit("notes", "aaaaaaaaaaaa");
        let plain = serde_json::to_value(&h).expect("serialize");
        assert!(
            plain.get("code_hints").is_none(),
            "empty list must be ABSENT"
        );
        assert!(plain.get("code_hints_total").is_none(), "0 must be ABSENT");
        assert!(plain.get("drift_open").is_none(), "0 must be ABSENT");

        h.code_hints = vec!["src/a.rs".to_string()];
        h.code_hints_total = 3;
        h.drift_open = 2;
        let marked = serde_json::to_value(&h).expect("serialize");
        assert_eq!(
            marked.get("code_hints"),
            Some(&serde_json::json!(["src/a.rs"]))
        );
        assert_eq!(marked.get("code_hints_total"), Some(&serde_json::json!(3)));
        assert_eq!(marked.get("drift_open"), Some(&serde_json::json!(2)));
    }

    // --- CT-C1 — the triage-side `.review/` directory scan -----------------

    #[test]
    fn flagged_reasons_in_dir_returns_empty_when_the_dir_does_not_exist_yet() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out = flagged_reasons_in_dir(&tmp.path().join("no-such-review-dir"));
        assert!(out.is_empty());
    }

    #[test]
    fn flagged_reasons_in_dir_only_counts_open_flag_comments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = test_paths(tmp.path());
        write_review_with_comment(
            &paths,
            "notes",
            "aaaaaaaaaaaa",
            "[kb-flag] the salience is stale",
            kb_core::review::CommentStatus::Open,
        );
        write_review_with_comment(
            &paths,
            "notes",
            "bbbbbbbbbbbb",
            "[kb-flag] resolved already",
            kb_core::review::CommentStatus::Resolved,
        );
        write_review_with_comment(
            &paths,
            "notes",
            "cccccccccccc",
            "not a flag at all",
            kb_core::review::CommentStatus::Open,
        );

        let kb_name = KbName::new("notes").expect("valid kb name");
        let out = flagged_reasons_in_dir(&paths.kb_review_dir(&kb_name));

        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            out.get("aaaaaaaaaaaa").map(String::as_str),
            Some("the salience is stale")
        );
    }

    // --- CT-E4 — the census `?sort=unverified` ordering --------------------

    /// Sort `(id, recall_count, never_opened)` rows through the EXACT
    /// comparator the census handler uses.
    fn sorted_unverified(mut rows: Vec<(&str, u32, bool)>) -> Vec<(&str, u32, bool)> {
        rows.sort_by(|a, b| unverified_cmp(*a, *b));
        rows
    }

    /// The agent-hot-human-cold bucket (recalled AND never opened) leads,
    /// recall_count DESC orders both within the bucket and across the cold
    /// remainder.
    #[test]
    fn census_unverified_cmp_buckets_agent_hot_human_cold_first() {
        let rows = sorted_unverified(vec![
            ("aa", 0, true),  // cold: never recalled
            ("bb", 5, false), // cold: recalled but opened
            ("cc", 2, true),  // HOT (2 recalls, never opened)
            ("dd", 9, true),  // HOT (9 recalls, never opened)
            ("ee", 9, false), // cold: recalled but opened
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec!["dd", "cc", "ee", "bb", "aa"]);
    }

    /// The bucket needs BOTH signals: an opened row never enters it however
    /// often it was recalled, and a never-opened row with zero recalls
    /// stays cold. A hot row with ONE recall still beats an opened row
    /// with many — the bucket dominates the count.
    #[test]
    fn census_unverified_cmp_requires_both_signals_for_the_bucket() {
        let rows = sorted_unverified(vec![
            ("cold-unrecalled", 0, true),
            ("opened-often-recalled", 7, false),
            ("hot-once", 1, true),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(
            ids,
            vec!["hot-once", "opened-often-recalled", "cold-unrecalled"]
        );
    }

    /// Ties break on id ASC — the census's default paging order — so the
    /// sort is fully deterministic, and a corpus with NO signals at all
    /// (no ledger rows, nothing opened) degrades to the default id-ASC
    /// order EXACTLY (the "absent sort is byte-identical" guarantee's
    /// degenerate twin).
    #[test]
    fn census_unverified_cmp_ties_break_on_id_asc_the_default_order() {
        // Equal (bucket, count) → id ASC, in both buckets.
        let rows = sorted_unverified(vec![
            ("zz", 3, true),
            ("aa", 3, true),
            ("yy", 0, true),
            ("bb", 0, true),
        ]);
        let ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec!["aa", "zz", "bb", "yy"]);

        // No signals anywhere ⇒ the default id-ASC order, untouched.
        let rows = sorted_unverified(vec![("cc", 0, true), ("aa", 0, true), ("bb", 0, true)]);
        let ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
        assert_eq!(ids, vec!["aa", "bb", "cc"]);
    }

    /// `never_opened_by_human` treats an ABSENT latest-visit entry and a
    /// recorded-but-never-scrolled one (pct == 0) alike — the exact CT-B6
    /// SPA condition (`read_pct` absent/0), so badge and bucket agree.
    #[test]
    fn never_opened_by_human_treats_absent_and_zero_pct_alike() {
        assert!(never_opened_by_human(None));
        assert!(never_opened_by_human(Some(&(0, None, 100))));
        assert!(!never_opened_by_human(Some(&(1, None, 100))));
        assert!(!never_opened_by_human(Some(&(
            90,
            Some("intro".to_string()),
            100
        ))));
    }

    // --- CT-B2 — the `visible_to` recall filter ----------------------------
    //
    // Full-daemon fixture-based route tests, mirroring the `l7_recall_*`
    // style in `tests/end_to_end.rs` (that file is owned by a different
    // task; these live here instead so the CT-B2 coverage stays inside the
    // files this task owns). Memories are dropped straight onto disk with
    // hand-written `<meta>` tags (rather than through the `POST
    // …/artifacts` convenience route, which defaults `global: true` when
    // both link fields are absent) specifically so the "unlinked" fixture
    // is a GENUINE zero-rows memory — `MemoryLinkSeedHook::enrich` only
    // ever writes a `memory_links` row when `kb-global` or `kb-linked-kbs`
    // is present (`crates/kb-core/src/enrich.rs`).

    fn vt_kb_section(
        path: std::path::PathBuf,
        memory_scope: Option<&str>,
    ) -> kb_core::config::KbSection {
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
            templates: std::collections::BTreeMap::new(),
            memory_scope: memory_scope.map(str::to_string),
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

    /// Boot a daemon with ONE `global`-scope memory corpus ("vismem")
    /// seeded, at boot, with four memories written directly to disk (no
    /// HTTP round trip): an unlinked one, a `kb-global` one, one linked
    /// only to "alpha", and one linked only to "beta". "alpha"/"beta" are
    /// plain link-target strings here, not configured kbs — the seed hook
    /// never validates a linked kb name against the daemon's config (only
    /// the `POST …/artifacts` and PUT-links MUTATION routes do that), so
    /// the fixture doesn't need them to exist.
    async fn boot_visible_to_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("mem");
        std::fs::create_dir_all(&mem).unwrap();

        std::fs::write(
            mem.join("unlinked.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <title>Unlinked Note</title>
            </head><body><p>quasar visibility everywhere</p></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            mem.join("global.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <meta name="kb-global" content="true">
                <title>Global Note</title>
            </head><body><p>quasar visibility everywhere</p></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            mem.join("alpha.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <meta name="kb-linked-kbs" content="alpha">
                <title>Alpha Note</title>
            </head><body><p>quasar visibility alpha-only</p></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            mem.join("beta.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <meta name="kb-linked-kbs" content="beta">
                <title>Beta Note</title>
            </head><body><p>quasar visibility beta-only</p></body></html>"#,
        )
        .unwrap();

        let daemon_name = format!(
            "vismem-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        );
        let mut kb_map: std::collections::BTreeMap<KbName, kb_core::config::KbSection> =
            std::collections::BTreeMap::new();
        kb_map.insert(
            KbName::new("vismem").unwrap(),
            vt_kb_section(mem, Some("global")),
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
        vt_wait_for_titles(
            addr,
            &["Unlinked Note", "Global Note", "Alpha Note", "Beta Note"],
        )
        .await;
        vt_wait_for_links(addr, &[("Alpha Note", "alpha"), ("Beta Note", "beta")]).await;
        (tmp, addr)
    }

    /// Boot a daemon with ONE `global`-scope memory corpus seeded with two
    /// memories, NEITHER carrying any `kb-global`/`kb-linked-kbs` meta —
    /// the "no memory is linked" fixture the absent-vs-present byte-
    /// identical test needs.
    async fn boot_visible_to_all_unlinked_fixture() -> (tempfile::TempDir, std::net::SocketAddr) {
        let tmp = tempfile::tempdir().unwrap();
        let mem = tmp.path().join("mem");
        std::fs::create_dir_all(&mem).unwrap();
        std::fs::write(
            mem.join("first.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <title>First Unlinked</title>
            </head><body><p>krypton absent-param probe</p></body></html>"#,
        )
        .unwrap();
        std::fs::write(
            mem.join("second.html"),
            r#"<html><head>
                <meta name="kb-category" content="memory-user">
                <title>Second Unlinked</title>
            </head><body><p>krypton absent-param probe</p></body></html>"#,
        )
        .unwrap();

        let daemon_name = format!(
            "vismem-unlinked-{}",
            tmp.path().file_name().unwrap().to_string_lossy()
        );
        let mut kb_map: std::collections::BTreeMap<KbName, kb_core::config::KbSection> =
            std::collections::BTreeMap::new();
        kb_map.insert(
            KbName::new("vismem").unwrap(),
            vt_kb_section(mem, Some("global")),
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
        vt_wait_for_titles(addr, &["First Unlinked", "Second Unlinked"]).await;
        (tmp, addr)
    }

    fn vt_url(addr: std::net::SocketAddr, path: &str) -> String {
        format!("http://{addr}{path}")
    }

    /// Poll `GET /api/kb/vismem/docs` until every title in `want` has
    /// landed, or a 30s deadline elapses (self-contained — `tests/common`'s
    /// `poll_until` lives in a different compilation unit and isn't
    /// reachable from a `src/` unit test).
    async fn vt_wait_for_titles(addr: std::net::SocketAddr, want: &[&str]) {
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let docs: Vec<serde_json::Value> = client
                .get(vt_url(addr, "/api/kb/vismem/docs?limit=50"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap_or_default();
            let titles: HashSet<&str> = docs.iter().filter_map(|d| d["title"].as_str()).collect();
            if want.iter().all(|t| titles.contains(t)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {want:?} to be indexed; got {titles:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// The link table is written by a post-index hook, so a memory's `title`
    /// can be listed (what `vt_wait_for_titles` proves) BEFORE its
    /// `kb-linked-kbs` edges exist. The recall route's `visible_to` filter
    /// reads that table at query time and treats an UNLINKED memory as
    /// visible — so a query that races the hook sees a beta-only note leak
    /// into alpha's view. Seen on the CI runner (attempt 2 of run
    /// 33969820667, 2026-09-05): "visible_to=alpha must EXCLUDE the beta-only
    /// memory; got [.., \"Beta Note\", ..]". Same class as kb-code's
    /// `hierarchy::implementors_smoke_json` (a doc/symbol count is a proxy
    /// that lands before the derived rows). Wait for the edges themselves.
    async fn vt_wait_for_links(addr: std::net::SocketAddr, want: &[(&str, &str)]) {
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let docs: Vec<serde_json::Value> = client
                .get(vt_url(addr, "/api/kb/vismem/docs?limit=50"))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap_or_default();
            let id_of = |title: &str| -> Option<String> {
                docs.iter()
                    .find(|d| d["title"].as_str() == Some(title))
                    .and_then(|d| d["id"].as_str().map(str::to_string))
            };
            let mut all_linked = true;
            for (title, kb) in want {
                let linked = match id_of(title) {
                    Some(id) => {
                        let r: serde_json::Value = client
                            .get(vt_url(addr, &format!("/api/kb/vismem/memories/{id}/links")))
                            .send()
                            .await
                            .unwrap()
                            .json()
                            .await
                            .unwrap_or_default();
                        r["linked_kbs"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(kb)))
                    }
                    None => false,
                };
                all_linked &= linked;
            }
            if all_linked {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for memory link edges {want:?} to be recorded"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn vt_recall_titles(addr: std::net::SocketAddr, query_suffix: &str) -> Vec<String> {
        let client = reqwest::Client::new();
        let r: serde_json::Value = client
            .get(vt_url(
                addr,
                &format!("/api/memory/recall?q=visibility&scope=all&limit=10{query_suffix}"),
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        r["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["title"].as_str().unwrap().to_string())
            .collect()
    }

    /// (a) An unlinked memory (no `memory_links` rows at all) passes under
    /// ANY `visible_to` value — unlinked memories are the shared commons,
    /// visible everywhere, the inverse of `for_kb`'s strict allowlist.
    #[tokio::test]
    async fn visible_to_unlinked_memory_passes_under_any_value() {
        let (_tmp, addr) = boot_visible_to_fixture().await;
        let titles = vt_recall_titles(addr, "&visible_to=zzz-not-a-real-kb").await;
        assert!(
            titles.contains(&"Unlinked Note".to_string()),
            "an unlinked memory must pass visible_to regardless of its value; got {titles:?}"
        );
    }

    /// (b) A `*`-linked (global) memory passes under ANY `visible_to`
    /// value, same as it does under `for_kb`.
    #[tokio::test]
    async fn visible_to_global_sentinel_memory_always_passes() {
        let (_tmp, addr) = boot_visible_to_fixture().await;
        let titles = vt_recall_titles(addr, "&visible_to=zzz-not-a-real-kb").await;
        assert!(
            titles.contains(&"Global Note".to_string()),
            "a `*`-linked memory must pass visible_to regardless of its value; got {titles:?}"
        );
    }

    /// (c) + (d) A memory linked ONLY to "alpha" passes when `visible_to`
    /// contains "alpha", and is DROPPED when `visible_to` names some other
    /// kb (here "beta") that isn't in its link set.
    #[tokio::test]
    async fn visible_to_named_kb_passes_only_when_it_intersects_the_link_set() {
        let (_tmp, addr) = boot_visible_to_fixture().await;

        let alpha_scoped = vt_recall_titles(addr, "&visible_to=alpha").await;
        assert!(
            alpha_scoped.contains(&"Alpha Note".to_string()),
            "visible_to=alpha must include the alpha-linked memory; got {alpha_scoped:?}"
        );
        assert!(
            !alpha_scoped.contains(&"Beta Note".to_string()),
            "visible_to=alpha must EXCLUDE the beta-only memory; got {alpha_scoped:?}"
        );

        let beta_scoped = vt_recall_titles(addr, "&visible_to=beta").await;
        assert!(
            beta_scoped.contains(&"Beta Note".to_string()),
            "visible_to=beta must include the beta-linked memory; got {beta_scoped:?}"
        );
        assert!(
            !beta_scoped.contains(&"Alpha Note".to_string()),
            "visible_to=beta must EXCLUDE the alpha-only memory; got {beta_scoped:?}"
        );
    }

    /// (e) `visible_to` composes with `for_kb` as an AND, not an OR: the
    /// alpha-only memory must survive ONLY when both filters individually
    /// allow it, and gets dropped the moment either one disagrees.
    #[tokio::test]
    async fn visible_to_composes_with_for_kb_as_an_and() {
        let (_tmp, addr) = boot_visible_to_fixture().await;

        // for_kb allows alpha, visible_to disagrees (names only beta) —
        // must still be dropped.
        let mismatched = vt_recall_titles(addr, "&for_kb=alpha&visible_to=beta").await;
        assert!(
            !mismatched.contains(&"Alpha Note".to_string()),
            "for_kb=alpha & visible_to=beta must still drop the alpha-only memory (AND, not OR); got {mismatched:?}"
        );

        // visible_to allows alpha, for_kb disagrees (names only beta) —
        // must still be dropped.
        let reversed = vt_recall_titles(addr, "&for_kb=beta&visible_to=alpha").await;
        assert!(
            !reversed.contains(&"Alpha Note".to_string()),
            "for_kb=beta & visible_to=alpha must still drop the alpha-only memory (AND, not OR); got {reversed:?}"
        );

        // Both filters agree on alpha — the memory survives.
        let agreed = vt_recall_titles(addr, "&for_kb=alpha&visible_to=alpha").await;
        assert!(
            agreed.contains(&"Alpha Note".to_string()),
            "for_kb=alpha & visible_to=alpha must both allow the alpha-only memory; got {agreed:?}"
        );
    }

    /// (f) Absent `visible_to` is byte-identical to the pre-CT-B2 shape:
    /// on a fixture where NO memory is linked at all, a recall with the
    /// param omitted and one with it explicitly set both return the exact
    /// same hits (id, order, and every field) — proving the filter is a
    /// true no-op whenever there's nothing to filter, and that an absent
    /// param never behaves differently from a present-but-harmless one.
    #[tokio::test]
    async fn visible_to_absent_is_byte_identical_when_nothing_is_linked() {
        let (_tmp, addr) = boot_visible_to_all_unlinked_fixture().await;
        let client = reqwest::Client::new();

        let without_param: serde_json::Value = client
            .get(vt_url(
                addr,
                "/api/memory/recall?q=krypton&scope=all&limit=10",
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let with_param: serde_json::Value = client
            .get(vt_url(
                addr,
                "/api/memory/recall?q=krypton&scope=all&limit=10&visible_to=some-unrelated-kb",
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();

        assert_eq!(
            without_param["hits"], with_param["hits"],
            "an unrelated visible_to must be a byte-identical no-op when nothing is linked"
        );
        // Sanity: both calls actually returned the two seeded memories —
        // an empty-vs-empty comparison would pass vacuously.
        assert_eq!(without_param["hits"].as_array().unwrap().len(), 2);
    }
}
