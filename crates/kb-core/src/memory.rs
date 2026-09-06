//! Agent-memory subsystem core (v0.9, track M): the LLM-free pieces the
//! daemon route and the CLI share.
//!
//! [`rerank`] is the pure, deterministic recall ranking function. Given
//! per-corpus hit lists tagged with their in-corpus rank position, it
//! scores each memory by `rank-position × salience × recency-decay`,
//! drops superseded/forgotten memories, de-dups, and returns the top
//! `limit`. (`render_artifact` — the shared `kb remember` HTML builder —
//! lands here in M4.)
//!
//! There is deliberately NO model/intelligence here: the live agent
//! decides what to keep; kb only stores, ranks, and serves.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::review::{Anchor, Author};

/// RRF-style rank constant: a hit at 0-based position `r` in its corpus
/// contributes relevance `1/(RANK_K + r)`. Matches lance's RRF default.
const RANK_K: f32 = 60.0;
/// Salience used when a memory carries no `kb-salience` meta. `pub` so the
/// recall route's inline decay-floor (`routes/memory.rs`) reads the SAME
/// default instead of duplicating the `0.5` literal — the two drifting apart
/// would silently change which memories the floor drops.
pub const DEFAULT_SALIENCE: f32 = 0.5;
/// Per-day decay rate for the two `kb-decay` buckets.
const DECAY_SLOW: f32 = 0.01;
const DECAY_FAST: f32 = 0.1;
const SECONDS_PER_DAY: f32 = 86_400.0;

/// MI-W4.0 — the ONE predicate for "does this doc count as a memory
/// artifact", shared by `recall`, `census`, and `dupes`. A memory-scoped
/// corpus's docs ALL participate in recall regardless of their specific
/// `kb-category` value — a memory need not carry a `"memory-*"` category
/// (the live `.kb-memory` corpus has rows tagged e.g. `"project"`) — recall
/// excludes only `kb-session`/`memory-session` transcripts
/// ([`crate::sessions::MEMORY_SESSION_CATEGORY`]). Before MI-W4.0, census
/// and dupes independently required `category.starts_with("memory-")`,
/// which silently hid every non-`"memory-"`-categoried row from the very
/// report meant to audit the recallable population. `census`/`dupes` MUST
/// filter with this same function so the two surfaces can't drift apart
/// again — see `memory_census_and_dupes_track_recall_eligibility` and
/// `recall` in `kb-server/src/routes/memory.rs`.
pub fn is_recallable_memory_category(category: Option<&str>) -> bool {
    category != Some(crate::sessions::MEMORY_SESSION_CATEGORY)
}

/// CT-C1 — the in-session correction verb: an agent that discovers a
/// recalled memory is WRONG has `kb memory flag <id> --reason "…"`, which
/// rides kb-comments/1 (invariant #6) rather than inventing new storage —
/// it POSTs an ordinary `author: claude` comment whose body starts with
/// this literal tag through the EXISTING `add_comment` route. This
/// constant plus its two pure helpers below are the ONE place that
/// recognizes/writes the tag, shared by the CLI (writes it), the triage
/// route (surfaces `reason_kind: "flagged"`), and the recall route
/// (surfaces `flagged: true`) — so all three agree on one grammar. There is
/// deliberately no new field on [`RecallHit`]/[`Scored`]: a flag is
/// SURFACED post-rank only, over the returned page (kb-server's `recall`
/// route), never fed into this module's scoring formula at all.
pub const FLAG_COMMENT_PREFIX: &str = "[kb-flag] ";

/// `true` iff `body` is a flag comment (starts with [`FLAG_COMMENT_PREFIX`]).
pub fn is_flag_comment(body: &str) -> bool {
    body.starts_with(FLAG_COMMENT_PREFIX)
}

/// The reason text after the tag, trimmed. `None` when `body` isn't a flag
/// comment at all (see [`is_flag_comment`]) — never a guessed excerpt.
pub fn flag_reason(body: &str) -> Option<&str> {
    body.strip_prefix(FLAG_COMMENT_PREFIX).map(str::trim)
}

/// CT-C4 — the drift-comment grammar, sibling of [`FLAG_COMMENT_PREFIX`].
/// `/kb-verify` (the agent-layer sweep skill,
/// `plugins/kb-memory/skills/kb-verify/SKILL.md`) files a dated
/// `[kb-drift] <path> — <human explanation>` comment on a memory whose
/// code citation no longer resolves against a live checkout — riding
/// kb-comments/1 (invariant #6) exactly like CT-C1's flag, at most ONE
/// open comment per (memory, cited path). This constant plus the two pure
/// helpers below are the ONE place that recognizes the tag, shared by the
/// skill's dedup gate and the recall route (which surfaces `drift_open`,
/// the OPEN-drift count per hit). `[kb-drift]` ≠ `[kb-flag]`: drift says
/// *the citation rotted* (the fact may still be true); a flag says the
/// fact itself is disputed — the prefixes are disjoint, so one body can
/// never be both. SURFACED, NEVER SCORED (the CT-C1 posture): nothing
/// lands on [`RecallHit`]/[`Scored`]; kb-server derives the count strictly
/// post-rank over the returned page.
pub const DRIFT_COMMENT_PREFIX: &str = "[kb-drift] ";

/// `true` iff `body` is a drift comment (starts with
/// [`DRIFT_COMMENT_PREFIX`]).
pub fn is_drift_comment(body: &str) -> bool {
    body.starts_with(DRIFT_COMMENT_PREFIX)
}

/// The cited path token after the tag: everything up to the first
/// whitespace or em-dash (the skill's body grammar is
/// `[kb-drift] <path> — <prose>`, and the machine-greppable half is
/// exactly the `<path>` token). `None` when `body` isn't a drift comment
/// at all, or when no path token follows the tag — never a guessed path
/// (the /kb-verify "honest states, never guesses" rule applies to the
/// parse-back too).
pub fn drift_comment_path(body: &str) -> Option<&str> {
    let rest = body.strip_prefix(DRIFT_COMMENT_PREFIX)?.trim_start();
    let token = rest
        .split(|c: char| c.is_whitespace() || c == '—')
        .next()
        .unwrap_or("");
    (!token.is_empty()).then_some(token)
}

/// CT-C3 — negative memory: "X was tried and FAILED" is the highest-value
/// wrong-action preventer recall can deliver, and it must not render like a
/// positive fact. A failed-outcome memory is an ORDINARY memory (same
/// ingest path, same corpus file, same scoring — invariant #10 untouched)
/// carrying a paired declaration, both written by `render_artifact` at
/// `kb remember --failed` time:
///
/// 1. **`<meta name="kb-outcome" content="failed">`** — the durable,
///    human-readable declaration in the artifact source. Written ONLY when
///    the outcome is failed; there is deliberately no `content="ok"` form
///    (absence is the ok state — an affirmation meta on every ordinary
///    memory would be pure noise, and kb never retro-classifies).
/// 2. **The [`FAILED_OUTCOME_TAG`] appended to `kb-tags`** — the INDEXED
///    carrier. The meta itself has no lance column and never will (zero new
///    storage): the existing tags pipeline (`parser::slugify_tag` →
///    `tags_csv` → `DocSummary.tags`) is what carries the fact into recall
///    (`RecallHit.warns` on the wire — kb-server computes it POST-rank from
///    the tag), census (`failed`), and the gallery for free (filterable via
///    the existing `?tags=` grammar as [`FAILED_OUTCOME_TAG_SLUG`]).
///
/// SURFACED, NEVER SCORED (the CT-C1 `flagged` posture): nothing here lands
/// on [`RecallHit`]/[`Scored`] — a failed memory ranks EXACTLY like the same
/// memory without the marker; the recall route derives its `warns` bool
/// strictly after `rerank_with_policy_scored` ran. This enum plus the
/// constants/helpers below are the ONE grammar shared by the CLI
/// (`--failed`), the ingest route (validation + tag pairing), the recall
/// route (`warns`), and the census (`failed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryOutcome {
    /// The approach this memory records was tried and did NOT work.
    Failed,
}

impl MemoryOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryOutcome::Failed => "failed",
        }
    }

    /// Loose case-insensitive parse of the (currently one-value) closed
    /// set. `None` for anything else — callers (the CLI, the ingest route)
    /// reject rather than silently coerce, same rule as [`MemoryType`].
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "failed" => Some(MemoryOutcome::Failed),
            _ => None,
        }
    }
}

/// CT-C3 — the tag spelling written INTO `kb-tags` at `--failed` write
/// time (human-readable in the source meta).
pub const FAILED_OUTCOME_TAG: &str = "outcome:failed";

/// CT-C3 — what the indexer actually stores for [`FAILED_OUTCOME_TAG`]:
/// `parser::slugify_tag` folds the `:` to `-`, so `DocSummary.tags` (and
/// the `?tags=` gallery grammar) carry THIS spelling. Pinned against the
/// real slugifier by `slugify_pins_the_failed_outcome_tag_slug` so the two
/// constants can never drift.
pub const FAILED_OUTCOME_TAG_SLUG: &str = "outcome-failed";

/// `true` iff a `kb-outcome` meta VALUE declares a failed outcome — the
/// parse-back twin of `render_artifact`'s write (one grammar, via
/// [`MemoryOutcome::parse`]). `None`/anything else is the ordinary
/// (non-failed) state.
pub fn is_failed_outcome(outcome: Option<&str>) -> bool {
    outcome
        .map(str::trim)
        .and_then(MemoryOutcome::parse)
        .is_some()
}

/// `true` iff `tag` is the failed-outcome tag in EITHER spelling — the raw
/// [`FAILED_OUTCOME_TAG`] as written into the source meta, or the
/// [`FAILED_OUTCOME_TAG_SLUG`] the indexer stores. Case-insensitive,
/// trimmed, mirroring `slugify_tag`'s own lowercase fold.
pub fn is_failed_outcome_tag(tag: &str) -> bool {
    let t = tag.trim().to_ascii_lowercase();
    t == FAILED_OUTCOME_TAG || t == FAILED_OUTCOME_TAG_SLUG
}

/// `true` iff `tags` carries the failed-outcome tag (either spelling) —
/// what the recall route's post-rank `warns` pass and the census `failed`
/// column both call against `DocSummary.tags`.
pub fn has_failed_outcome_tag(tags: &[String]) -> bool {
    tags.iter().any(|t| is_failed_outcome_tag(t))
}

/// Append [`FAILED_OUTCOME_TAG`] unless a failed-outcome tag (either
/// spelling) is already present — idempotent, so the CLI (which pairs the
/// tag client-side for graceful degradation against an older daemon that
/// ignores the unknown `outcome` field) and the ingest route (the
/// authoritative pairing) can BOTH call it without ever double-tagging.
pub fn ensure_failed_outcome_tag(tags: &mut Vec<String>) {
    if !has_failed_outcome_tag(tags) {
        tags.push(FAILED_OUTCOME_TAG.to_string());
    }
}

/// v0.10 M1 — auto-decay aggressiveness for the corpus. Drives the
/// salience floor `rerank` applies BEFORE the recency-decay math.
/// Pinned memories survive every policy level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecayPolicy {
    /// Drop memories with salience ≤ 0.20.
    Strict,
    /// Drop memories with salience ≤ 0.15. The shipped default.
    #[default]
    Balanced,
    /// Never auto-drop; only explicit `kb forget` removes a memory.
    Loose,
}

impl DecayPolicy {
    /// Lower-bound salience threshold for auto-drop. A memory with
    /// salience ≤ threshold is filtered out at recall time (unless
    /// pinned).
    pub fn drop_threshold(self) -> f32 {
        match self {
            DecayPolicy::Strict => 0.20,
            DecayPolicy::Balanced => 0.15,
            // `f32::NEG_INFINITY` keeps every memory (no salience can
            // ever be ≤ NEG_INFINITY).
            DecayPolicy::Loose => f32::NEG_INFINITY,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DecayPolicy::Strict => "strict",
            DecayPolicy::Balanced => "balanced",
            DecayPolicy::Loose => "loose",
        }
    }

    /// Loose case-insensitive parse. Returns `None` for unknown values;
    /// the route uses that to surface a 400. (Named `parse` rather
    /// than `from_str` to sidestep clippy's `should_implement_trait`
    /// lint — implementing `FromStr` here would also need an `Err`
    /// type and isn't currently used by any trait-bound caller.)
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "strict" => Some(DecayPolicy::Strict),
            "balanced" => Some(DecayPolicy::Balanced),
            "loose" => Some(DecayPolicy::Loose),
            _ => None,
        }
    }
}

/// MI-W3.3a — the CoALA-minimal memory-type taxonomy (Sumers et al.,
/// "Cognitive Architectures for Language Agents"): episodic (what
/// happened), semantic (facts/preferences), procedural (how-to). An
/// OPTIONAL `<meta name="kb-memory-type">` — absent (untyped) is the vast
/// majority of the existing corpus and stays that way: kb never INFERS a
/// type and never backfills one onto an existing memory. Stored as a plain
/// `Option<String>` at the schema/lance layer (matching `kb_decay`'s own
/// free-form-string precedent) — this enum exists for CLI validation
/// (`--type`) and display, not as a wire/storage type of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryType {
    /// What happened — an event, a decision, a specific episode.
    Episodic,
    /// A fact or standing preference, true independent of any one event.
    Semantic,
    /// How to do something — a recipe, a workflow, a skill.
    Procedural,
}

impl MemoryType {
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryType::Episodic => "episodic",
            MemoryType::Semantic => "semantic",
            MemoryType::Procedural => "procedural",
        }
    }

    /// Loose case-insensitive parse of the closed set. `None` for anything
    /// else — callers (the CLI arg parser, the ingest route) reject rather
    /// than silently coerce, since a typo'd type would otherwise persist
    /// forever (kb never backfills/corrects it later).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "episodic" => Some(MemoryType::Episodic),
            "semantic" => Some(MemoryType::Semantic),
            "procedural" => Some(MemoryType::Procedural),
            _ => None,
        }
    }
}

/// MI-W3.4 — write-time trust tagging: where the CONTENT originally came
/// from, as distinct from [`MemoryProvenance::source_kb`]/`source_artifact`
/// (an intra-kb copy origin). An OPTIONAL `<meta name="kb-source">`.
/// SURFACED, NEVER SCORED — see `provenance_cannot_reach_the_scorer`, which
/// this variant extends explicitly. `fetched-web` is the untrusted-origin
/// value the write-time default-scope gate (`kb remember --source`, CLI
/// side) keys off of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrustSource {
    /// Pulled from a URL / web page the agent fetched — untrusted origin.
    FetchedWeb,
    /// The human said it directly (dictation, a chat message, an
    /// explicit instruction).
    UserDictated,
    /// The agent inferred/derived it (e.g. from reading code), rather
    /// than being told it directly.
    AgentInference,
}

impl TrustSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TrustSource::FetchedWeb => "fetched-web",
            TrustSource::UserDictated => "user-dictated",
            TrustSource::AgentInference => "agent-inference",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "fetched-web" => Some(TrustSource::FetchedWeb),
            "user-dictated" => Some(TrustSource::UserDictated),
            "agent-inference" => Some(TrustSource::AgentInference),
            _ => None,
        }
    }
}

/// One search hit fed into the re-ranker. `rank` is the 0-based position
/// within the hit's source corpus result list; the signal fields are read
/// straight off the widened (M1) search projection.
#[derive(Debug, Clone)]
pub struct RecallHit {
    pub kb: String,
    pub id: String,
    pub title: String,
    pub path: String,
    pub rank: usize,
    pub salience: Option<f32>,
    pub decay: Option<String>,
    pub mtime_unix: Option<i64>,
    pub status: Option<String>,
    /// v0.10 M1 — set by the route when this memory id is in the kb's
    /// `pinned_memories` set. Pinned memories survive every decay-policy
    /// floor.
    #[allow(dead_code)]
    pub pinned: bool,
    /// v0.14 T1 — origin Claude Code session id, projected through
    /// from the lance `kb_session` column. Passed through `rerank` so
    /// the recall route can surface a "from session: …" sub-line on
    /// each memory row without a follow-up fetch.
    pub session_id: Option<String>,
    /// RA4 — one-line summary projected from the `kb_summary` column,
    /// passed through so the recall route surfaces it without a refetch.
    pub summary: Option<String>,
    /// MI-W3.3a — optional CoALA-minimal type, projected through from
    /// `kb_memory_type`. Pure pass-through, exactly like `session_id`/
    /// `summary` above — `rerank`'s scoring formula never reads it.
    pub memory_type: Option<String>,
    /// MI-W3.4 — write-time trust tag, projected through from `kb_source`.
    /// Same pure-pass-through treatment: SURFACED, NEVER SCORED.
    pub source: Option<String>,
    /// CT-A1 (U3 parse-back) — the `you`/`claude` role, projected through
    /// from `kb_author`. Same pure-pass-through treatment as `memory_type`/
    /// `source` above — `rerank`'s scoring formula never reads it. Named
    /// like [`MemoryProvenance::author`], the write-side twin.
    pub author: Option<String>,
    /// CT-A1 — kb name of the artifact this memory was highlighted FROM,
    /// projected through from `kb_source_kb`. Same pure-pass-through
    /// treatment; named like [`MemoryProvenance::source_kb`].
    pub source_kb: Option<String>,
    /// CT-A1 — artifact id of that origin artifact, projected through from
    /// `kb_source_artifact`. Same pure-pass-through treatment; named like
    /// [`MemoryProvenance::source_artifact`].
    pub source_artifact: Option<String>,
    /// CT-A1 — the origin selection's `review::Anchor` JSON text, projected
    /// through from `kb_source_anchor`. Same pure-pass-through treatment;
    /// named like [`MemoryProvenance::source_anchor`] (though this is the
    /// raw JSON text, not a parsed `Anchor`).
    pub source_anchor: Option<String>,
    /// MI-W2.1 — the raw search-engine relevance score for this hit
    /// (`DocSummary.score`): hybrid `_relevance_score`, BM25 `_score`, or
    /// vector `1/(1+distance)`, whichever the route's query arm produced.
    /// `None` on the empty-query/list_docs timeline path (no score at
    /// all) — `rerank_with_policy_scored` must treat that exactly like
    /// "nothing to normalize" (see [`Scored::relevance_factor`]), never a
    /// zero. Ignored entirely unless `scoring_v2` is on.
    pub score: Option<f32>,
    /// MI-W2.2 — how many times a `kb-recall` hook has actually injected
    /// this memory into a captured session, summed across the daemon's
    /// `memory_recalls` ledger (W1). Populated by the recall route ONLY
    /// when `scoring_v2` is on (an extra fan-out the default-off path
    /// skips); `0` otherwise, which is indistinguishable from "genuinely
    /// never recalled" — both mean [`stability_multiplier`] is a no-op.
    pub recall_count: u32,
    /// MI-W2.2 — the most recent `memory_recalls.recalled_at` for this
    /// memory (unix seconds), across every kb (max). `None` when
    /// `recall_count == 0` or the read was skipped (`scoring_v2` off).
    pub last_recalled_at: Option<i64>,
}

/// A ranked recall result.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub kb: String,
    pub id: String,
    pub title: String,
    pub path: String,
    pub score: f32,
    pub salience: f32,
    /// v0.14 T1 — passed through from `RecallHit.session_id`.
    pub session_id: Option<String>,
    /// RA4 — passed through from `RecallHit.summary`.
    pub summary: Option<String>,
    /// MI-W3.3a — passed through from `RecallHit.memory_type`.
    pub memory_type: Option<String>,
    /// MI-W3.4 — passed through from `RecallHit.source`.
    pub source: Option<String>,
    /// CT-A1 (U3 parse-back) — passed through from `RecallHit.author`.
    pub author: Option<String>,
    /// CT-A1 — passed through from `RecallHit.source_kb`.
    pub source_kb: Option<String>,
    /// CT-A1 — passed through from `RecallHit.source_artifact`.
    pub source_artifact: Option<String>,
    /// CT-A1 — passed through from `RecallHit.source_anchor`.
    pub source_anchor: Option<String>,
    /// invariant:10 decomposition — the 0-based in-corpus rank position
    /// `rel` was computed from. Surfaced (never re-scored) so a caller can
    /// render the arithmetic without re-deriving it.
    pub rank: u32,
    /// invariant:10 decomposition — `1/(60+rank)` (`RANK_K`-scaled rank
    /// relevance), the first factor of `score`.
    pub rel: f32,
    /// invariant:10 decomposition — `exp(-k*age_days)`, the recency-decay
    /// factor (third factor of `score`).
    pub decay: f32,
    /// invariant:10 decomposition — age in days the `decay` factor was
    /// computed against (0.0 when the hit has no mtime, i.e. undecayed).
    pub age_days: f32,
    /// MI-W2.1 decomposition — the per-corpus min-max normalized search
    /// relevance factor `score` was ADDITIONALLY multiplied by (on top of
    /// `rel`), in `[0,1]`. `None` when `scoring_v2` is off; `Some(1.0)`
    /// (a neutral no-op) when the hit's own corpus can't support a
    /// meaningful spread (its score is absent, it's the corpus's only
    /// scored hit, or every hit ties) — see
    /// [`compute_relevance_factors`].
    pub relevance_factor: Option<f32>,
    /// MI-W2.2 decomposition — the FSRS-inspired stability multiplier
    /// `decay` was scaled by before being capped at 1.0. `None` when
    /// `scoring_v2` is off; `Some(1.0)` (a no-op) for a never-recalled
    /// memory even when the flag is on — see [`stability_multiplier`].
    pub stability: Option<f32>,
    /// MI-W4.1 decomposition — the per-day decay RATE (`decay_k`'s return
    /// value for this hit's `kb-decay` bucket: `0.01` slow, `0.1` fast)
    /// `decay`'s exponent was computed from. Surfaced (never re-derived by
    /// a caller guessing the bucket→rate mapping) so the `/memory` health-
    /// timeline sparkline can project the SAME SCORE curve forward in time
    /// (`salience × exp(-k·age)` for age = `age_days + t`) without
    /// duplicating the two magic constants client-side — see
    /// `web/src/lib/decayProjection.ts`. **This curve is a RANKING signal
    /// only** — it never determines whether the memory is excluded from
    /// recall (that's [`FloorState`], evaluated against the raw, undecayed
    /// `salience` above); see [`decay_half_life_days`] for the one true
    /// fact this rate implies on its own.
    pub decay_k: f32,
}

/// Per-day decay rate `k` for a `kb-decay` value. Absent/unknown → slow.
/// `pub` (MI-W4.1) so a caller projecting the decay curve forward in time
/// (the [`Scored::decay_k`] wire field's source of truth, and
/// [`decay_half_life_days`]) reads the SAME two constants `rerank`
/// actually scores with, rather than a second guess at the bucket→rate
/// mapping drifting out of sync.
pub fn decay_k(decay: Option<&str>) -> f32 {
    match decay {
        Some("fast") => DECAY_FAST,
        _ => DECAY_SLOW,
    }
}

/// MI-W4.1(revision) — GROUND TRUTH this module must never contradict:
/// **decay never lowers the value the salience floor tests.** The floor
/// (both enforcement sites — this file's `rerank_with_policy_scored` filter
/// chain, and the recall route's inline per-hit check) compares against the
/// RAW, UNDECAYED `kb-salience` meta; `exp(-k·age)` is applied ONLY to the
/// ranking `score`. A memory whose author-set salience is above the floor
/// is therefore NEVER dropped from recall by ageing — not in 9 days, not
/// ever. It only ranks lower and lower until it stops making the top-N. The
/// ONLY true evictions are: salience already at/under the floor (a fact
/// true from the moment it was set, not a future event you can count down
/// to), an explicit `kb forget` tombstone, or a supersede.
///
/// The previous revision of this module shipped a `days_until_floor_
/// crossing` helper that solved `salience × exp(-k·(age+t)) = floor` for
/// `t` and presented that as "days until this memory drops" — asserting an
/// event that cannot happen under the actual filter above (see the removed
/// function's git history + the MI-W4.1 revision report for the full
/// post-mortem). It has been deleted, not renamed: there is no honest
/// "days until eviction" quantity to compute, because eviction isn't
/// time-driven. What replaces it:
///
/// - [`floor_state`] — the TRUE, non-time-varying answer to "is this memory
///   excluded from recall right now": a STATE (`above`/`below`/pinned/
///   no-floor derived from the CONSTANT raw salience), never a date.
/// - [`decay_half_life_days`] — the TRUE fact about the SCORE trajectory
///   (`exp(-k·age)` really does shrink the ranking score over time, which
///   really does make a memory progressively less likely to surface in the
///   top-N): a fixed half-life derived directly from `decay_k`, independent
///   of salience/age/floor.
pub fn decay_half_life_days(decay_k: f32) -> f32 {
    if decay_k > 0.0 {
        std::f32::consts::LN_2 / decay_k
    } else {
        f32::INFINITY
    }
}

/// MI-W4.1(revision) — the four TRUE states a memory's RAW salience can be
/// in relative to the active decay-policy floor, evaluated exactly the way
/// [`rerank_with_policy_scored`]'s own filter chain does (`salience <=
/// floor`, unpinned) — never against a decayed/projected value. This is a
/// STATE, not a projection: raw salience is constant, so (barring an
/// explicit `kb memory salience` edit) a memory's `FloorState` does not
/// change with the passage of time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FloorState {
    /// Pinned memories bypass the floor at every policy level — checked
    /// FIRST, before any salience comparison.
    Pinned,
    /// The active policy is `Loose` (`floor = None`) — nothing is ever
    /// excluded on salience grounds.
    NoFloor,
    /// `salience > floor` — recallable today (and, per the ground truth
    /// above, will remain so for as long as this salience value stands).
    Above { salience: f32, floor: f32 },
    /// `salience <= floor` — excluded from recall RIGHT NOW, not at some
    /// future date.
    Below { salience: f32, floor: f32 },
}

/// Pure classifier — see [`FloorState`]. `floor = None` represents the
/// `Loose` policy's `f32::NEG_INFINITY` (never put on the wire as a literal
/// non-finite float, matching [`DecayPolicy::drop_threshold`]'s callers).
pub fn floor_state(salience: f32, floor: Option<f32>, pinned: bool) -> FloorState {
    if pinned {
        return FloorState::Pinned;
    }
    let Some(floor) = floor else {
        return FloorState::NoFloor;
    };
    if salience <= floor {
        FloorState::Below { salience, floor }
    } else {
        FloorState::Above { salience, floor }
    }
}

/// Pure recall re-ranker — deterministic for a given `(hits, tombstones,
/// now_unix, limit)`:
///
/// - drops any hit whose id is in `tombstones` (the superseded ids) or
///   whose status is `"forgotten"`,
/// - scores `rel × salience × exp(-k · age_days)` where `rel =
///   1/(60+rank)`, `salience` defaults to `0.5`, `age_days` comes from
///   `mtime_unix` (None → age 0, i.e. undecayed), and `k` from the decay
///   bucket — `Scored` surfaces `rank`/`rel`/`decay`/`age_days` alongside
///   `score` so a caller can render the arithmetic without re-deriving it
///   (invariant #10: surfaced, never re-scored),
/// - de-dups by id keeping the highest score (a memory could surface from
///   more than one corpus),
/// - sorts by score desc (id asc tie-break) and truncates to `limit`.
pub fn rerank(
    hits: Vec<RecallHit>,
    tombstones: &HashSet<String>,
    now_unix: i64,
    limit: usize,
) -> Vec<Scored> {
    // M1 default — use the daemon-default policy. Callers that own a
    // per-kb policy go through [`rerank_with_policy`] instead.
    rerank_with_policy(hits, tombstones, now_unix, limit, DecayPolicy::default())
}

/// As [`rerank`], but honors a `DecayPolicy` that drops low-salience
/// (non-pinned) memories before scoring. Pinned memories pass through
/// the floor at every policy level.
///
/// Delegates to [`rerank_with_policy_scored`] with BOTH v2 flags `false` —
/// kept as a SEPARATE, untouched entry point (rather than threading flags
/// through every existing call site) specifically so this function's
/// output stays byte-identical to pre-MI-W2 kb by construction, not by
/// convention: `rerank_with_policy_scored(_, false, false)` executes the
/// exact pre-W2 arithmetic (see its doc comment).
pub fn rerank_with_policy(
    hits: Vec<RecallHit>,
    tombstones: &HashSet<String>,
    now_unix: i64,
    limit: usize,
    policy: DecayPolicy,
) -> Vec<Scored> {
    rerank_with_policy_scored(hits, tombstones, now_unix, limit, policy, false, false)
}

/// MI-W2.1/MI-W2.2, split MI-W5.R — invariant #10's v2 scoring line,
/// flag-gated by TWO independent flags (`[memory] scoring_v2_relevance` /
/// `scoring_v2_stability` in kb.toml — **default `true`/`false`**
/// respectively, see [`crate::config::MemorySection`] for why they're no
/// longer one flag). Each factor is applied ONLY when its own flag is on;
/// all four `(relevance, stability)` combinations are valid.
///
/// Both flags `false` reproduces [`rerank_with_policy`]'s pre-W2 formula
/// EXACTLY: `score = rel × salience × decay`, with `relevance_factor` and
/// `stability` left `None` on every `Scored`. Not "multiplied by a neutral
/// 1.0" — the multiplication is skipped entirely, so the floating-point
/// result is bit-for-bit the old value (no rounding could ever creep in).
///
/// `scoring_v2_relevance = true` folds in the MI-W2.1 relevance factor: each
/// hit's raw search-engine score (`RecallHit.score` — hybrid/BM25/vector, or
/// `None` on the empty-query timeline) is min-max normalized to `[0,1]`
/// against its OWN corpus's other SURVIVING hits in this call
/// ([`compute_relevance_factors`], run AFTER the tombstone/forgotten/
/// salience-floor filter chain — a hit about to be dropped can never skew a
/// survivor's normalization; MI-W2.R review fix), then folded in as a second
/// multiplicative factor alongside `rel`: `score = rel × salience × decay ×
/// relevance_factor`. A corpus that can't support a meaningful spread (a
/// `None` score, the corpus's only scored hit, or a tie) degrades to a
/// neutral `1.0` — never a divide-by-zero, never an arbitrary tie-break
/// award. **Measured** on the live corpus (W5.1 bench) and ON by default.
///
/// `scoring_v2_stability = true` folds in the MI-W2.2 stability factor:
/// `decay` is scaled by [`stability_multiplier`] (derived from the hit's own
/// `recall_count`/`last_recalled_at`, i.e. the W1 `memory_recalls` ledger)
/// and capped at `1.0` — a memory that keeps getting actually injected into
/// sessions decays more slowly, but can never stop decaying altogether (see
/// that function's doc for the bound + the "never immortal" property).
/// **Unmeasured** on the live corpus (the W5.1 bench never loaded the
/// sessions corpus the `memory_recalls` ledger lives in, so every hit had
/// `recall_count == 0`) — fixture-tested only, OFF by default.
///
/// Both factors are surfaced on `Scored` independently (never silently
/// absorbed into `score` alone) — `relevance_factor` is `Some` iff
/// `scoring_v2_relevance` is on, `stability` is `Some` iff
/// `scoring_v2_stability` is on — so `kb recall --explain` can render the
/// full breakdown of whichever factors are actually active.
pub fn rerank_with_policy_scored(
    hits: Vec<RecallHit>,
    tombstones: &HashSet<String>,
    now_unix: i64,
    limit: usize,
    policy: DecayPolicy,
    scoring_v2_relevance: bool,
    scoring_v2_stability: bool,
) -> Vec<Scored> {
    let floor = policy.drop_threshold();

    // MI-W2.R (review fix) — the drop filters run FIRST, and
    // `compute_relevance_factors` is computed over the SURVIVING set only.
    // Originally this was computed over the raw `hits` slice before the
    // tombstone/forgotten/salience-floor filter chain below — but the
    // underlying search queries never exclude forgotten or tombstoned
    // docs, so a memory that was ABOUT TO BE DROPPED still contributed its
    // raw engine score to its corpus's min/max, skewing every survivor's
    // normalized `relevance_factor`. With soft-forget (MI-W2.3) now
    // KEEPING forgotten rows in the index (rather than hard-deleting them
    // out of existence), a forgotten sibling sitting in the hit list is
    // the NORMAL case, not a rare edge — so this ordering bug would fire
    // on every recall against a corpus with any forgotten/tombstoned
    // memories. Filtering first means a dropped sibling can never reach
    // [`compute_relevance_factors`] at all, so it can't skew anyone's
    // normalization — a survivor's `relevance_factor` is now IDENTICAL to
    // what it would be had the dropped sibling never been indexed.
    let filtered: Vec<RecallHit> = hits
        .into_iter()
        .filter(|h| !tombstones.contains(&h.id))
        .filter(|h| h.status.as_deref() != Some("forgotten"))
        // M1 — auto-decay drop. Pinned memories always survive.
        .filter(|h| {
            if h.pinned {
                return true;
            }
            let s = h.salience.unwrap_or(DEFAULT_SALIENCE);
            s > floor
        })
        .collect();

    // MI-W2.1 — computed over the POST-FILTER survivor set (see above),
    // grouped by each hit's OWN corpus (`kb`) — a hit's spread is defined
    // by its corpus-mates only, never the merged cross-corpus set. Indices
    // now key into `filtered`, not the original `hits`. Skipped entirely
    // when the flag is off (no reason to pay for a HashMap pass nothing
    // will read).
    let relevance_factors: Vec<f32> = if scoring_v2_relevance {
        compute_relevance_factors(&filtered)
    } else {
        Vec::new()
    };

    let mut scored: Vec<Scored> = filtered
        .into_iter()
        .enumerate()
        .map(|(i, h)| {
            let rank = h.rank as u32;
            let rel = 1.0 / (RANK_K + h.rank as f32);
            let salience = h.salience.unwrap_or(DEFAULT_SALIENCE);
            let k = decay_k(h.decay.as_deref());
            let age_days = h
                .mtime_unix
                .map(|m| ((now_unix - m).max(0) as f32) / SECONDS_PER_DAY)
                .unwrap_or(0.0);
            let base_decay = (-k * age_days).exp();

            let (decay, stability) = if scoring_v2_stability {
                let s = stability_multiplier(
                    h.recall_count,
                    h.last_recalled_at,
                    h.mtime_unix,
                    salience,
                    h.decay.as_deref(),
                );
                ((base_decay * s).min(1.0), Some(s))
            } else {
                (base_decay, None)
            };

            let (score, relevance_factor) = if scoring_v2_relevance {
                let rf = relevance_factors[i];
                (rel * salience * decay * rf, Some(rf))
            } else {
                (rel * salience * decay, None)
            };

            Scored {
                kb: h.kb,
                id: h.id,
                title: h.title,
                path: h.path,
                score,
                salience,
                session_id: h.session_id,
                summary: h.summary,
                memory_type: h.memory_type,
                source: h.source,
                author: h.author,
                source_kb: h.source_kb,
                source_artifact: h.source_artifact,
                source_anchor: h.source_anchor,
                rank,
                rel,
                decay,
                age_days,
                relevance_factor,
                stability,
                decay_k: k,
            }
        })
        .collect();

    // Highest score first; id ascending as a deterministic tie-break.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    // A memory could surface from more than one corpus; keep the first
    // occurrence, which is the highest-scoring after the sort.
    let mut seen = HashSet::new();
    scored.retain(|s| seen.insert(s.id.clone()));

    scored.truncate(limit);
    scored
}

/// MI-W2.1 — per-corpus (kb) min-max normalization of `RecallHit.score`
/// onto `[0,1]`, returned as a `Vec` parallel to `hits` (same length, same
/// order — index `i` is hit `i`'s factor). A corpus's spread is defined by
/// its OWN hits only (per-corpus grouping by `kb`), matching the
/// requirement that a hit's normalization never depends on which OTHER
/// corpora happened to be in scope for this call.
///
/// **Callers MUST pass only the POST-FILTER survivor set** (MI-W2.R review
/// fix) — [`rerank_with_policy_scored`] runs the tombstone/forgotten/
/// salience-floor drop chain first and calls this over what's left, never
/// the raw hit list. A hit that's about to be dropped still carries a raw
/// engine score; letting it into this function would skew the min/max
/// every survivor is normalized against, even though that hit will never
/// appear in the result. This matters in practice, not just in theory:
/// with soft-forget (MI-W2.3) the underlying search queries don't exclude
/// forgotten/tombstoned docs, so they show up in `hits` routinely.
///
/// Degrades to a neutral `1.0` (never a divide-by-zero, never an arbitrary
/// tie-break) for any hit whose corpus can't support a meaningful spread:
///
/// - the hit's own `score` is `None` (the empty-query/list_docs timeline
///   path carries no score at all), or
/// - its corpus contributed fewer than two SCORED hits to this call, or
/// - every scored hit in its corpus ties (max == min).
fn compute_relevance_factors(hits: &[RecallHit]) -> Vec<f32> {
    let mut by_kb: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, h) in hits.iter().enumerate() {
        if h.score.is_some() {
            by_kb.entry(h.kb.as_str()).or_default().push(i);
        }
    }
    let mut out = vec![1.0_f32; hits.len()];
    for idxs in by_kb.into_values() {
        if idxs.len() < 2 {
            continue; // already 1.0 — nothing to normalize against.
        }
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &i in &idxs {
            let s = hits[i].score.expect("filtered to Some above");
            min = min.min(s);
            max = max.max(s);
        }
        if (max - min).abs() < f32::EPSILON {
            continue; // a tie — already 1.0.
        }
        for &i in &idxs {
            let s = hits[i].score.expect("filtered to Some above");
            out[i] = (s - min) / (max - min);
        }
    }
    out
}

/// MI-W2.2 ratified constants — subject to W5 bench tuning; the whole
/// scoring_v2 line stays flag-gated OFF by default until then.
const STABILITY_GAIN_BASE: f32 = 0.15;
/// Hard ceiling on the stability multiplier — see [`stability_multiplier`]
/// for why this keeps a memory from ever becoming immortal.
const STABILITY_CEILING: f32 = 3.0;

/// MI-W2.2 — an FSRS-inspired stability multiplier, closed-form and pure:
/// a function of ONLY the memory's own aggregate recall-ledger stats
/// (`recall_count`, `last_recalled_at` — both from W1's `memory_recalls`
/// table via `memory_recalls_counts_for_ids`), its `created` timestamp,
/// its `salience`, and its `decay` bucket. No iteration over raw ledger
/// rows happens here or anywhere in the hot path — the caller already
/// reduced them to that one `(count, last_recalled_at)` pair per memory.
///
/// **The idea (Bjork desirable difficulty, as FSRS formalizes it):** a
/// review that lands when a memory is ALMOST forgotten teaches more than
/// one that lands while it's still fresh. We don't have FSRS's true
/// per-review elapsed-time series — W1's aggregate keeps only a COUNT and
/// the LATEST recall's timestamp — so this approximates "how close to its
/// decay/salience floor was this memory at the moment it was actually
/// recalled" using that latest recall as a stand-in for the average
/// recall, and applies it uniformly across `recall_count` events:
///
/// ```text
/// decay_at_recall = exp(-k · age_at_last_recall_days)      // (0,1]
/// difficulty      = 0.5·(1 − decay_at_recall) + 0.5·(1 − salience)   // [0,1)
/// gain            = recall_count × STABILITY_GAIN_BASE × difficulty
/// stability       = min(1 + gain, STABILITY_CEILING)
/// ```
///
/// `difficulty` blends TWO floors, both named in the ratified spec: how
/// decayed the memory was (age-based) and how close to the salience floor
/// it sits (a low-salience memory that keeps surviving real recalls is
/// stronger evidence than a high-salience one being recalled again).
///
/// The caller folds `stability` into decay as `min(base_decay × stability,
/// 1.0)` — a straight multiplicative scale-up, capped so `decay` never
/// exceeds its normal `(0,1]` range.
///
/// **Guaranteed properties** (see the `stability_*` test table):
///
/// - **monotonic**: holding every other input fixed, `stability` never
///   DECREASES as `recall_count` increases (`gain` is a non-negative
///   multiple of `recall_count`, and `min(..)` preserves monotonicity).
/// - **bounded, never immortal**: `stability <= STABILITY_CEILING` always
///   — a finite multiplier on `k` can slow an exponential decay curve but
///   can never halt it: as `age_days → ∞`, `base_decay → 0`, so
///   `base_decay × stability → 0` regardless of how large (but finite)
///   `stability` is. A memory can survive proportionally longer; it can
///   never stop decaying.
/// - **never-recalled is unchanged**: `recall_count == 0` returns exactly
///   `1.0` (short-circuited, not merely "gain happens to be zero") — a
///   memory nobody has ever actually recalled decays EXACTLY as it did
///   before MI-W2.2, byte for byte.
pub fn stability_multiplier(
    recall_count: u32,
    last_recalled_at: Option<i64>,
    created_unix: Option<i64>,
    salience: f32,
    decay_bucket: Option<&str>,
) -> f32 {
    if recall_count == 0 {
        return 1.0;
    }
    let k = decay_k(decay_bucket);
    let age_at_recall_days = match (last_recalled_at, created_unix) {
        (Some(recalled), Some(created)) if recalled > created => {
            ((recalled - created) as f32) / SECONDS_PER_DAY
        }
        _ => 0.0,
    };
    let decay_at_recall = (-k * age_at_recall_days).exp();
    let salience_clamped = salience.clamp(0.0, 1.0);
    let difficulty = 0.5 * (1.0 - decay_at_recall) + 0.5 * (1.0 - salience_clamped);
    let gain = recall_count as f32 * STABILITY_GAIN_BASE * difficulty;
    (1.0 + gain).min(STABILITY_CEILING)
}

/// Wrap plain text as escaped HTML paragraphs (blank lines split
/// paragraphs; single newlines become `<br>`). Turns a
/// `kb remember "<text>"` body into the artifact's inner-body markup.
pub fn text_to_body_html(text: &str) -> String {
    text.split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("<p>{}</p>", escape_text(p).replace('\n', "<br>\n")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// U3 — where a memory CAME FROM, recorded structurally in the memory
/// artifact's own source.
///
/// The SPA's "highlight → save as memory" action (`SelectionActions.tsx`)
/// is the first memory write that is unambiguously a HUMAN gesture rather
/// than an agent `kb remember`, so it records three things the agent path
/// never had: the source artifact (kb + artifact id), the selection
/// [`Anchor`] the text was lifted from, and the [`Author`] role.
///
/// Three rules govern this record, and they are load-bearing:
///
/// 1. **It rides the existing ingest path.** There is no provenance store,
///    no provenance table, and no second write path — these are more
///    `<meta>` tags in the same `render_artifact` output that `kb remember`
///    has always produced, written by the same `routes::artifacts::ingest`.
/// 2. **`author` is the `you | claude` ROLE split, not an identity.** kb is
///    one daemon, one operator (README → Non-goals); there is deliberately
///    no user record, no owner column, and no per-user state anywhere near
///    this struct. `Author` is reused verbatim from `review` — the same two
///    values a comment carries.
/// 3. **Provenance is a SURFACED signal, never a score term.** [`rerank`]
///    does not see it: none of these metas feed `salience`/`decay`/
///    `kb-created`, so a human-sourced memory ranks EXACTLY like an agent-
///    sourced one — same rule invariant #11's R3 applies to recollect
///    staleness. Since CT-A1 (v0.38) ALL FIVE provenance metas — `author`/
///    `source_kb`/`source_artifact`/`source_anchor` (once write-only) and
///    `source` (MI-W3.4's trust tag) — ARE parsed back and ride
///    `RecallHit`/`Scored` as pure pass-through DISPLAY fields (same
///    treatment as `session_id`/`summary`), never read by the scoring
///    formula itself. `provenance_cannot_reach_the_scorer` pins every one
///    of them out of the arithmetic.
///
/// Every field is optional: an ordinary `kb remember` passes `None` for the
/// whole struct and the rendered HTML is byte-identical to pre-U3.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryProvenance {
    /// `you` (a human wrote/kept this) or `claude` (an agent did). Emitted
    /// as `<meta name="kb-author">`; absent ⇒ no meta, which is how every
    /// pre-U3 memory reads (unattributed, not "claude by default" — we do
    /// not retro-attribute what we did not record).
    pub author: Option<Author>,
    /// kb name of the artifact the text was lifted from.
    pub source_kb: Option<String>,
    /// Artifact id (12-hex, path-derived) of that artifact.
    pub source_artifact: Option<String>,
    /// The selection this memory was cut from — a `review::Anchor`, the
    /// SAME anchor shape highlights/comments/list entries already use
    /// (invariant #25). There is exactly one anchor grammar in kb; this
    /// does not invent a second one, and it serialises through
    /// `lists::anchor_to_json` so the JSON is canonical everywhere.
    pub source_anchor: Option<Anchor>,
    /// MI-W3.4 — write-time TRUST tag: where the CONTENT itself originally
    /// came from (fetched-web / user-dictated / agent-inference), as
    /// distinct from `source_kb`/`source_artifact` above (an intra-kb COPY
    /// origin — where this text was lifted FROM within kb, which is
    /// orthogonal to whether the original content is trustworthy). Emitted
    /// as `<meta name="kb-source">`. SURFACED, NEVER SCORED — see
    /// `provenance_cannot_reach_the_scorer`.
    pub source: Option<TrustSource>,
}

impl MemoryProvenance {
    /// `true` when nothing is recorded — the caller can skip building the
    /// struct entirely (and `render_artifact` emits no provenance metas).
    pub fn is_empty(&self) -> bool {
        self.author.is_none()
            && self.source_kb.is_none()
            && self.source_artifact.is_none()
            && self.source_anchor.is_none()
            && self.source.is_none()
    }
}

/// Meta value for an [`Author`] — matches the serde `rename_all =
/// "lowercase"` wire form so the meta and the review-file JSON agree.
fn author_meta(a: Author) -> &'static str {
    match a {
        Author::You => "you",
        Author::Claude => "claude",
    }
}

/// Build a memory artifact's HTML — a minimal valid `<!DOCTYPE html>`
/// document carrying the kb-* metas the indexer reads. `body_html` is the
/// ready inner-body markup (use [`text_to_body_html`] for plain text).
/// Shared by the ingest route and `kb remember` so both emit the same
/// shape; the produced HTML round-trips through `parser::extract`.
#[allow(clippy::too_many_arguments)]
pub fn render_artifact(
    title: &str,
    body_html: &str,
    category: &str,
    tags: &[String],
    salience: Option<f32>,
    decay: Option<&str>,
    supersedes: Option<&str>,
    session: Option<&str>,
    global: bool,
    linked_kbs: &[String],
    // RA3 — write-time creation timestamp (unix seconds), emitted as
    // `<meta name="kb-created">`. Because it lives in the source it is
    // re-parsed on every reindex, so a memory's decay basis stays stable
    // across rewrites (unlike filesystem mtime/btime). The ingest route
    // passes `Some(now)`; tests pass `None`.
    created: Option<i64>,
    // RA4 — one-line summary distinct from the title, emitted as
    // `<meta name="kb-summary">` and surfaced on recall hits.
    summary: Option<&str>,
    // U3 — where this memory came from (source artifact + selection anchor
    // + author role). `None` (or an empty record) emits nothing, keeping
    // the `kb remember` output byte-identical to pre-U3. See
    // [`MemoryProvenance`] for the three rules this record obeys.
    provenance: Option<&MemoryProvenance>,
    // MI-W3.3a — optional CoALA-minimal classification, emitted as
    // `<meta name="kb-memory-type">`. `None` (the vast majority of the
    // corpus) emits nothing — kb never infers or backfills a type.
    memory_type: Option<MemoryType>,
    // CT-C3 — negative-memory outcome, emitted as
    // `<meta name="kb-outcome" content="failed">` ONLY when
    // `Some(MemoryOutcome::Failed)`; `None` (every ordinary memory) emits
    // nothing — there is no "outcome: ok" noise meta. Callers writing this
    // pair it with the `outcome:failed` kb-tag (`ensure_failed_outcome_tag`,
    // the indexed carrier) — see [`MemoryOutcome`] for the full pairing.
    outcome: Option<MemoryOutcome>,
) -> String {
    let title_esc = escape_text(title);
    let mut metas = format!(
        "<meta name=\"kb-category\" content=\"{}\">",
        escape_attr(category)
    );
    if !tags.is_empty() {
        metas.push_str(&format!(
            "\n<meta name=\"kb-tags\" content=\"{}\">",
            escape_attr(&tags.join(", "))
        ));
    }
    if let Some(s) = salience {
        metas.push_str(&format!("\n<meta name=\"kb-salience\" content=\"{s}\">"));
    }
    if let Some(d) = decay {
        metas.push_str(&format!(
            "\n<meta name=\"kb-decay\" content=\"{}\">",
            escape_attr(d)
        ));
    }
    if let Some(sup) = supersedes {
        metas.push_str(&format!(
            "\n<meta name=\"kb-supersedes\" content=\"{}\">",
            escape_attr(sup)
        ));
    }
    if let Some(sid) = session {
        metas.push_str(&format!(
            "\n<meta name=\"kb-session\" content=\"{}\">",
            escape_attr(sid)
        ));
    }
    // RA3 — write-time decay basis. Stable across reindex (it's in the
    // source), so a rewrite/reindex no longer resets the memory's age.
    if let Some(ts) = created {
        metas.push_str(&format!("\n<meta name=\"kb-created\" content=\"{ts}\">"));
    }
    // RA4 — one-line summary distinct from the title.
    if let Some(sum) = summary {
        metas.push_str(&format!(
            "\n<meta name=\"kb-summary\" content=\"{}\">",
            escape_attr(sum)
        ));
    }
    // L5 — memory visibility metas. `kb-global="true"` makes the
    // memory recallable from every kb (the `*` sentinel in V0010);
    // `kb-linked-kbs="a,b"` scopes it to an explicit kb list. The
    // indexer's first-time seed pass reads these into V0010; UI
    // mutations afterwards never touch the HTML.
    if global {
        metas.push_str("\n<meta name=\"kb-global\" content=\"true\">");
    }
    if !linked_kbs.is_empty() {
        metas.push_str(&format!(
            "\n<meta name=\"kb-linked-kbs\" content=\"{}\">",
            escape_attr(&linked_kbs.join(", "))
        ));
    }
    // U3 — provenance metas. Recorded, never ranked (see the struct doc):
    // nothing here reaches `rerank`, which only ever sees salience/decay/
    // mtime off `RecallHit`.
    if let Some(p) = provenance {
        if let Some(a) = p.author {
            metas.push_str(&format!(
                "\n<meta name=\"kb-author\" content=\"{}\">",
                author_meta(a)
            ));
        }
        if let Some(k) = p.source_kb.as_deref().filter(|s| !s.is_empty()) {
            metas.push_str(&format!(
                "\n<meta name=\"kb-source-kb\" content=\"{}\">",
                escape_attr(k)
            ));
        }
        if let Some(id) = p.source_artifact.as_deref().filter(|s| !s.is_empty()) {
            metas.push_str(&format!(
                "\n<meta name=\"kb-source-artifact\" content=\"{}\">",
                escape_attr(id)
            ));
        }
        if let Some(anchor) = p.source_anchor.as_ref() {
            // One anchor JSON grammar corpus-wide (#25) — `lists::
            // anchor_to_json`, the same serialization a list entry stores.
            metas.push_str(&format!(
                "\n<meta name=\"kb-source-anchor\" content=\"{}\">",
                escape_attr(&crate::lists::anchor_to_json(anchor))
            ));
        }
        // MI-W3.4 — write-time trust tag. Surfaced (census/recall), never
        // scored — see `provenance_cannot_reach_the_scorer`.
        if let Some(src) = p.source {
            metas.push_str(&format!(
                "\n<meta name=\"kb-source\" content=\"{}\">",
                src.as_str()
            ));
        }
    }
    // MI-W3.3a — optional CoALA-minimal type classification. Absent for
    // the vast majority of the corpus by design (never inferred/backfilled).
    if let Some(t) = memory_type {
        metas.push_str(&format!(
            "\n<meta name=\"kb-memory-type\" content=\"{}\">",
            t.as_str()
        ));
    }
    // CT-C3 — the durable failed-outcome declaration. Written only for
    // `Some(Failed)`; absence IS the ok state (never an "outcome: ok"
    // meta). The indexed carrier is the paired `outcome:failed` kb-tag,
    // which rides the ordinary tags pipeline above — see [`MemoryOutcome`].
    if let Some(o) = outcome {
        metas.push_str(&format!(
            "\n<meta name=\"kb-outcome\" content=\"{}\">",
            o.as_str()
        ));
    }
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n\
         <title>{title_esc}</title>\n{metas}\n</head>\n<body>\n<main>\n\
         <h1>{title_esc}</h1>\n{body_html}\n</main>\n</body>\n</html>\n"
    )
}

/// v0.13 D7 — strip the memory-specific metas (`kb-salience`,
/// `kb-decay`, `kb-pinned`, `kb-supersedes`) from a memory artifact's
/// HTML so it can be promoted to a regular (non-memory) artifact. The
/// `kb-category` / `kb-tags` / `<title>` / `<body>` are preserved
/// verbatim — the goal is "keep the content, drop the memory-corpus
/// signals."
///
/// Returns the stripped HTML. Idempotent — running it on already-
/// stripped HTML is a no-op.
pub fn strip_memory_metas(html: &str) -> String {
    // Cheap line-by-line strip — the renderer puts each meta on its own
    // line (see `render_artifact`), so a regex over the whole document
    // would be overkill. Falls through unchanged on lines that don't
    // match.
    let drop = |line: &str| -> bool {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("<meta") {
            return false;
        }
        // name="kb-salience" | kb-decay | kb-pinned | kb-supersedes
        trimmed.contains("name=\"kb-salience\"")
            || trimmed.contains("name=\"kb-decay\"")
            || trimmed.contains("name=\"kb-pinned\"")
            || trimmed.contains("name=\"kb-supersedes\"")
    };
    let mut out = String::with_capacity(html.len());
    for (i, line) in html.lines().enumerate() {
        if drop(line) {
            continue;
        }
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    // Preserve a trailing newline if the input had one.
    if html.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// MI-W2.3 — soft forget: splice `kb-status: forgotten` +
/// `kb-forgotten-at: <now_unix>` into an artifact's own source, tombstoning
/// it in place instead of deleting it. Reuses the two GENERIC byte-
/// preserving splices ([`crate::meta_edit::set_meta_content`] for HTML,
/// [`crate::markdown::set_frontmatter_field`] for Markdown) — this
/// function is the memory-specific CALLER that decides WHICH two metas to
/// write; `meta_edit` itself stays a general-purpose single-tag editor
/// (invariant: extend the caller, never weaken the generic splice, mirrors
/// `routes::artifacts::patch_meta`'s tags/category scoping).
///
/// The route reindexes after this write (the same "write-only, searchable
/// within one debounce" path every other source edit takes), which lights
/// up the existing (previously dead — zero producers) `status ==
/// "forgotten"` filter in [`rerank_with_policy_scored`]: a soft-forgotten
/// memory drops out of `recall` on the very next reindex, exactly as a
/// hard delete used to, but the file (and every OTHER consumer — census,
/// plain search, versions/diff) still sees it.
///
/// Idempotent in effect (re-forgetting just rewrites the same two values
/// with a fresh timestamp) — NOT a no-op splice, since `kb-forgotten-at`
/// is expected to change on each call.
pub fn mark_forgotten(src: &str, is_md: bool, now_unix: i64) -> String {
    let stamp = now_unix.to_string();
    if is_md {
        let with_status =
            crate::markdown::set_frontmatter_field(src, "kb-status", Some("forgotten"));
        crate::markdown::set_frontmatter_field(&with_status, "kb-forgotten-at", Some(&stamp))
    } else {
        let with_status = crate::meta_edit::set_meta_content(src, "kb-status", Some("forgotten"));
        crate::meta_edit::set_meta_content(&with_status, "kb-forgotten-at", Some(&stamp))
    }
}

/// MI-W3.2b — edit a memory's salience IN PLACE by splicing `kb-salience`
/// into its own source, reusing the SAME two generic byte-preserving
/// splices [`mark_forgotten`] uses ([`crate::meta_edit::set_meta_content`]
/// for HTML, [`crate::markdown::set_frontmatter_field`] for Markdown).
///
/// Memory metadata is otherwise IMMUTABLE via the API — `patch_meta`
/// (`routes::artifacts::patch_meta`) is scoped strictly to
/// `kb-tags`/`kb-category` and deliberately never touches the memory
/// metas. Salience is the one exception: the W4 hygiene queue and human
/// triage both need to retune "how important is this" without a full
/// supersede-and-rewrite. `salience` is clamped to `[0,1]` by the CALLER
/// (the route), mirroring `render_artifact`'s own write-time clamp — this
/// function trusts its input.
///
/// The route reindexes after this write (same "write-only, searchable
/// within one debounce" path every other source edit takes), so the new
/// salience is live for the next `recall`/`census` within one debounce.
pub fn set_salience(src: &str, is_md: bool, salience: f32) -> String {
    let val = salience.to_string();
    if is_md {
        crate::markdown::set_frontmatter_field(src, "kb-salience", Some(&val))
    } else {
        crate::meta_edit::set_meta_content(src, "kb-salience", Some(&val))
    }
}

/// MI-W3.1 — one memory's facts as fed into [`find_duplicate_pairs`]. The
/// caller (the `/api/memory/dupes` route) assembles this from a per-corpus
/// `list_docs` scan (title/forgotten/supersedes) joined against
/// `list_embeddings` (the vector) — see that route's doc comment for why
/// those two existing read-lane calls are the cheapest correct source and
/// no new storage message was warranted.
#[derive(Debug, Clone)]
pub struct DupeCandidate {
    pub kb: String,
    pub id: String,
    pub title: String,
    /// `true` when `kb-status == "forgotten"` (MI-W2.3 soft-forget). Dropped
    /// before comparison — a tombstoned memory is not a live duplicate.
    pub forgotten: bool,
    /// This memory's own `kb-supersedes` value, if any (the id it replaces).
    pub supersedes: Option<String>,
    /// `None` when the memory has no stored embedding (never indexed with
    /// one, or a kb with no `embedding_model` configured) — dropped before
    /// comparison, same as `forgotten`.
    pub embedding: Option<Vec<f32>>,
}

/// MI-W3.1 — one likely-redundant pair `find_duplicate_pairs` reports.
/// Plain internal type — the `/api/memory/dupes` route projects this into
/// its own ts-exported wire struct (mirrors how `RecallHit` here is
/// distinct from `routes::memory::RecallResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct DupePair {
    pub kb_a: String,
    pub id_a: String,
    pub title_a: String,
    pub kb_b: String,
    pub id_b: String,
    pub title_b: String,
    /// Cosine similarity between the two memories' embeddings, in `[-1,1]`
    /// (in practice `[0,1]` for the bge family — never negative in
    /// observed corpora, but the math itself doesn't rule it out).
    pub cosine: f32,
    /// `true` when `kb_a != kb_b` — the high-value case a same-corpus
    /// neighbor search structurally cannot see (the W0.4 re-home moved 117
    /// memories cross-corpus; ~72% of real supersede links turned out to
    /// be cross-corpus per the pre-build precision probe, operator ruling
    /// 2026-08-05).
    pub cross_corpus: bool,
}

/// MI-W3.1 default similarity band for "likely redundant" — see the module
/// doc comment on [`find_duplicate_pairs`] for the reasoning; exposed so
/// the route/CLI can document it inline (`--threshold`, default this
/// value).
pub const DEFAULT_DUPES_THRESHOLD: f32 = 0.90;

/// Cosine similarity between two equal-length vectors. Returns `0.0` on a
/// length mismatch (mismatched embedding dims — e.g. two corpora on
/// different `embedding_model`s — cannot be compared) or a zero vector,
/// rather than dividing by zero or panicking on a shape mismatch. Kept as
/// its own small pure function here (rather than reused from
/// `routes::atlas`'s private copy of the identical formula) so the whole
/// MI-W3.1 engine is unit-testable in kb-core without a storage actor.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// MI-W3.1 — pure, deterministic all-pairs duplicate finder over a fixed
/// set of memory candidates spanning ANY number of corpora (the caller
/// fans these in from every in-scope memory corpus, invariant #28).
///
/// This is deliberately NOT a contradiction detector — the operator ruling
/// that authorized this unit (2026-08-05) reports a pre-build precision
/// probe (30 sampled memories, 145 same-corpus neighbor pairs) that found
/// ZERO genuine contradictions at ANY cosine band, because both existing
/// memory corpora are append-only CHANGELOGS: high similarity there means
/// "same project, next phase," not "conflicting facts." What the probe DID
/// find is strong topical signal (95.7% related-or-duplicate once
/// tag-overlap >= 1) and — the reason this exists as an ON-DEMAND report
/// rather than nothing — that ~72% of REAL supersede links turned out to be
/// CROSS-corpus (the W0.4 re-home moved 117 memories from the project
/// corpus into the global one), a shape a same-corpus-only neighbor search
/// structurally cannot see. So: no hook, no auto-generated review comments,
/// no alert fatigue — just a report the operator runs by hand and resolves
/// with `kb remember --supersedes` / `kb forget`.
///
/// A pair is reported when ALL of:
/// - neither candidate is `forgotten` (a tombstoned memory is dead, not a
///   live duplicate to resolve),
/// - both carry a same-length embedding (mismatched dims — different
///   `embedding_model`s — can't be compared; the pair is skipped, not
///   scored as 0),
/// - the pair is NOT already linked by `kb-supersedes` in EITHER direction
///   (that relationship is already resolved — reporting it again would be
///   noise, not signal),
/// - cosine similarity >= `threshold`.
///
/// **Threshold default (`DEFAULT_DUPES_THRESHOLD = 0.90`) — the reasoning,
/// spelled out because no per-band numeric table survives from the
/// pre-build probe in this repo, only its aggregate conclusions (above).**
/// "Related-or-duplicate" (what the probe measured, and found abundant
/// even at a fairly loose tag-overlap gate) is a much WIDER, lower-cosine
/// band than genuine near-duplicate REDUNDANCY — the same fact restated.
/// Flooding this report with topically-adjacent-but-distinct memories
/// would recreate exactly the alert-fatigue failure mode that got the
/// original always-on conflict-candidate `EnrichmentHook` dropped. 0.90 is
/// a conservative, precision-biased starting point (bge-family cosine
/// similarity above ~0.9 is, in practice, "the same sentence again," not
/// merely "the same topic") — `--threshold` exists precisely so an
/// operator (or the W5 bench) can recalibrate it once the actual
/// duplicate/related boundary is measured empirically.
///
/// Sort: cosine descending, then `(kb_a, id_a, kb_b, id_b)` ascending —
/// deterministic for a given input set. Truncates to `limit`. NEVER
/// mutates anything — the caller (operator or agent) resolves a real
/// duplicate via `kb remember --supersedes` or `kb forget`.
pub fn find_duplicate_pairs(
    candidates: Vec<DupeCandidate>,
    threshold: f32,
    limit: usize,
) -> Vec<DupePair> {
    let mut live: Vec<DupeCandidate> = candidates
        .into_iter()
        .filter(|c| !c.forgotten && c.embedding.is_some())
        .collect();
    // Deterministic comparison order — the output order is also sorted
    // below, but a stable input order keeps the O(n^2) loop itself
    // reproducible (matters for e.g. a future "first N pairs" streaming cap).
    live.sort_by(|a, b| (a.kb.as_str(), a.id.as_str()).cmp(&(b.kb.as_str(), b.id.as_str())));

    let mut pairs = Vec::new();
    for i in 0..live.len() {
        for j in (i + 1)..live.len() {
            let a = &live[i];
            let b = &live[j];
            // Already-resolved relationship — not noise worth reporting.
            if a.supersedes.as_deref() == Some(b.id.as_str())
                || b.supersedes.as_deref() == Some(a.id.as_str())
            {
                continue;
            }
            let va = a.embedding.as_ref().expect("filtered to Some above");
            let vb = b.embedding.as_ref().expect("filtered to Some above");
            if va.len() != vb.len() {
                continue; // different embedding_model dims — incomparable.
            }
            let cosine = cosine_similarity(va, vb);
            if cosine < threshold {
                continue;
            }
            pairs.push(DupePair {
                kb_a: a.kb.clone(),
                id_a: a.id.clone(),
                title_a: a.title.clone(),
                kb_b: b.kb.clone(),
                id_b: b.id.clone(),
                title_b: b.title.clone(),
                cosine,
                cross_corpus: a.kb != b.kb,
            });
        }
    }

    pairs.sort_by(|x, y| {
        y.cosine
            .partial_cmp(&x.cosine)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                (
                    x.kb_a.as_str(),
                    x.id_a.as_str(),
                    x.kb_b.as_str(),
                    x.id_b.as_str(),
                )
                    .cmp(&(
                        y.kb_a.as_str(),
                        y.id_a.as_str(),
                        y.kb_b.as_str(),
                        y.id_b.as_str(),
                    ))
            })
    });
    pairs.truncate(limit);
    pairs
}

/// Filename-safe slug from a memory title (lowercase, alphanumeric runs
/// joined by `-`, trimmed, capped at 60 chars). Empty/garbage → "memory".
/// NOT unique on its own — the ingest route appends a timestamp + an
/// existence check for collision-resistance.
pub fn memory_slug(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let capped: String = out.trim_matches('-').chars().take(60).collect();
    let capped = capped.trim_matches('-').to_string();
    if capped.is_empty() {
        "memory".to_string()
    } else {
        capped
    }
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;
    const DAY: i64 = 86_400;

    // CT-C1 — the flag-comment grammar is pure string matching; no daemon,
    // no review file needed to pin it.
    #[test]
    fn is_flag_comment_recognizes_only_the_tagged_prefix() {
        assert!(is_flag_comment(
            "[kb-flag] this contradicts the other memory"
        ));
        assert!(!is_flag_comment("just an ordinary comment"));
        assert!(!is_flag_comment("kb-flag] missing the bracket"));
        assert!(!is_flag_comment(""));
    }

    #[test]
    fn flag_reason_strips_the_prefix_and_trims() {
        assert_eq!(
            flag_reason("[kb-flag]   the salience is stale  "),
            Some("the salience is stale")
        );
        assert_eq!(flag_reason("not a flag"), None);
        assert_eq!(flag_reason("[kb-flag] "), Some(""));
    }

    // CT-C4 — the drift-comment grammar, golden-pinned against /kb-verify's
    // OWN example bodies (plugins/kb-memory/skills/kb-verify/SKILL.md) so
    // the skill's filed comments and this parse-back can never drift apart.
    #[test]
    fn is_drift_comment_recognizes_only_the_tagged_prefix() {
        // SKILL.md Step 4's exact example body, verbatim.
        assert!(is_drift_comment(
            "[kb-drift] src/storage/actor.rs:1502-1516 — absent in kb@<short-sha> (swept 2026-08-21); was cited for the read-lane classification. Fact unverified, citation dead."
        ));
        // SKILL.md Step 5's exact flag-escalation body MENTIONS the tag but
        // doesn't start with it — an escalation is a flag, never drift.
        assert!(!is_drift_comment(
            "cited fn now does X, memory claims Y — see [kb-drift] comment"
        ));
        // The two prefixes are disjoint: a flag body is never drift and a
        // drift body is never a flag.
        assert!(!is_drift_comment("[kb-flag] the fact itself is wrong"));
        assert!(!is_flag_comment("[kb-drift] src/a.rs — moved"));
        assert!(!is_drift_comment("just an ordinary comment"));
        assert!(!is_drift_comment("kb-drift] missing the bracket"));
        assert!(!is_drift_comment(""));
    }

    #[test]
    fn drift_comment_path_extracts_the_path_token_up_to_whitespace_or_em_dash() {
        // SKILL.md Step 4's exact example body → exactly its `<path>` half.
        assert_eq!(
            drift_comment_path(
                "[kb-drift] src/storage/actor.rs:1502-1516 — absent in kb@<short-sha> (swept 2026-08-21); was cited for the read-lane classification. Fact unverified, citation dead."
            ),
            Some("src/storage/actor.rs:1502-1516")
        );
        // A tight em-dash (no surrounding space) still terminates the token.
        assert_eq!(
            drift_comment_path("[kb-drift] src/a.rs—rotted since v0.36"),
            Some("src/a.rs")
        );
        // Bare-path form (no prose tail at all).
        assert_eq!(
            drift_comment_path("[kb-drift] crates/kb-core/src/memory.rs"),
            Some("crates/kb-core/src/memory.rs")
        );
        // Not a drift comment → None, never a guessed excerpt.
        assert_eq!(
            drift_comment_path("cited fn now does X, memory claims Y — see [kb-drift] comment"),
            None
        );
        assert_eq!(drift_comment_path("[kb-flag] not drift"), None);
        // Tag with no path token after it → honest None, not Some("").
        assert_eq!(drift_comment_path("[kb-drift] "), None);
        assert_eq!(drift_comment_path("[kb-drift]  — prose only"), None);
    }

    // CT-C3 — the failed-outcome grammar (meta + tag pairing) is pure; no
    // daemon needed to pin it.
    #[test]
    fn memory_outcome_parse_recognizes_only_failed() {
        assert_eq!(MemoryOutcome::parse("failed"), Some(MemoryOutcome::Failed));
        assert_eq!(MemoryOutcome::parse("FAILED"), Some(MemoryOutcome::Failed));
        assert_eq!(MemoryOutcome::parse("ok"), None);
        assert_eq!(MemoryOutcome::parse("success"), None);
        assert_eq!(MemoryOutcome::parse(""), None);
        assert_eq!(MemoryOutcome::Failed.as_str(), "failed");
    }

    #[test]
    fn is_failed_outcome_reads_the_meta_value_through_the_one_grammar() {
        assert!(is_failed_outcome(Some("failed")));
        assert!(is_failed_outcome(Some("  Failed ")));
        assert!(!is_failed_outcome(Some("ok")));
        assert!(!is_failed_outcome(Some("")));
        assert!(!is_failed_outcome(None));
    }

    /// The write-time spelling and the indexed spelling are two constants;
    /// this golden pins them to the REAL slugifier so they can never drift
    /// (`?tags=outcome-failed` and `DocSummary.tags` both see the slug).
    #[test]
    fn slugify_pins_the_failed_outcome_tag_slug() {
        assert_eq!(
            crate::parser::slugify_tag(FAILED_OUTCOME_TAG),
            FAILED_OUTCOME_TAG_SLUG
        );
    }

    #[test]
    fn failed_outcome_tag_helpers_match_both_spellings_and_dedup() {
        assert!(is_failed_outcome_tag("outcome:failed"));
        assert!(is_failed_outcome_tag("outcome-failed"));
        assert!(is_failed_outcome_tag("  Outcome:Failed "));
        assert!(!is_failed_outcome_tag("outcome"));
        assert!(!is_failed_outcome_tag("failed"));
        assert!(!is_failed_outcome_tag("outcome:ok"));

        assert!(has_failed_outcome_tag(&[
            "rust".to_string(),
            "outcome-failed".to_string()
        ]));
        assert!(!has_failed_outcome_tag(&["rust".to_string()]));
        assert!(!has_failed_outcome_tag(&[]));

        // ensure_failed_outcome_tag is idempotent across BOTH spellings.
        let mut tags = vec!["rust".to_string()];
        ensure_failed_outcome_tag(&mut tags);
        assert_eq!(tags, vec!["rust".to_string(), "outcome:failed".to_string()]);
        ensure_failed_outcome_tag(&mut tags);
        assert_eq!(tags.len(), 2, "raw spelling already present — no dup");
        let mut slugged = vec!["outcome-failed".to_string()];
        ensure_failed_outcome_tag(&mut slugged);
        assert_eq!(slugged.len(), 1, "slug spelling already present — no dup");
    }

    /// CT-C3 — the meta is written ONLY for `Some(Failed)`; an ordinary
    /// memory's HTML carries no `kb-outcome` at all (absence IS the ok
    /// state — never an "outcome: ok" noise meta).
    #[test]
    fn render_artifact_writes_the_kb_outcome_meta_only_when_failed() {
        let failed = render_artifact(
            "Tried X",
            "<p>did not work</p>",
            "memory-user",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            Some(MemoryOutcome::Failed),
        );
        assert!(failed.contains("<meta name=\"kb-outcome\" content=\"failed\">"));

        let ordinary = render_artifact(
            "Plain fact",
            "<p>works fine</p>",
            "memory-user",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert!(!ordinary.contains("kb-outcome"));
    }

    /// CT-C3 round-trip: the paired write (meta + `outcome:failed` tag)
    /// comes back through the REAL parser with the tag in slug form —
    /// the indexed carrier recall `warns` / census `failed` read. The meta
    /// itself deliberately has NO parsed field (zero new storage): the
    /// tags pipeline is the parse-back path.
    #[test]
    fn failed_outcome_round_trips_through_the_tags_pipeline() {
        let mut tags = vec!["ports".to_string()];
        ensure_failed_outcome_tag(&mut tags);
        let html = render_artifact(
            "Binding 4000 locally FAILED",
            &text_to_body_html("prod owns 127.0.0.1:4000; use a spare port."),
            "memory-user",
            &tags,
            Some(0.6),
            None,
            None,
            None,
            true,
            &[],
            Some(1_700_000_000),
            Some("Never bind 4000 locally"),
            None,
            None,
            Some(MemoryOutcome::Failed),
        );
        let f = crate::parser::extract(&html);
        assert!(f.tags.contains(&FAILED_OUTCOME_TAG_SLUG.to_string()));
        assert!(has_failed_outcome_tag(&f.tags));
        assert!(f.tags.contains(&"ports".to_string()));
        assert!(html.contains("<meta name=\"kb-outcome\" content=\"failed\">"));
    }

    /// MI-W5.R — every `(scoring_v2_relevance, scoring_v2_stability)`
    /// combination, for fixtures that must hold regardless of which v2
    /// factors are active (the two flags are independent — see
    /// `rerank_with_policy_scored`'s doc). Order includes the shipped
    /// default (`true, false`) and the pre-split "both on" combination.
    const ALL_FOUR_FLAG_COMBINATIONS: [(bool, bool); 4] =
        [(false, false), (true, false), (false, true), (true, true)];

    fn hit(kb: &str, id: &str, rank: usize) -> RecallHit {
        RecallHit {
            kb: kb.into(),
            id: id.into(),
            title: format!("title-{id}"),
            path: format!("{id}.html"),
            rank,
            salience: None,
            decay: None,
            mtime_unix: None,
            status: None,
            pinned: false,
            session_id: None,
            summary: None,
            memory_type: None,
            source: None,
            author: None,
            source_kb: None,
            source_artifact: None,
            source_anchor: None,
            score: None,
            recall_count: 0,
            last_recalled_at: None,
        }
    }

    #[test]
    fn decay_policy_drop_threshold_matches_design() {
        assert_eq!(DecayPolicy::Strict.drop_threshold(), 0.20);
        assert_eq!(DecayPolicy::Balanced.drop_threshold(), 0.15);
        assert!(DecayPolicy::Loose.drop_threshold().is_infinite());
    }

    #[test]
    fn decay_policy_from_str_roundtrip() {
        for p in [
            DecayPolicy::Strict,
            DecayPolicy::Balanced,
            DecayPolicy::Loose,
        ] {
            assert_eq!(DecayPolicy::parse(p.as_str()), Some(p));
        }
        assert_eq!(DecayPolicy::parse("nope"), None);
    }

    #[test]
    fn strict_policy_drops_low_salience_memories() {
        let mut a = hit("g", "a", 0);
        a.salience = Some(0.10); // below 0.20 → dropped under Strict
        let mut b = hit("g", "b", 1);
        b.salience = Some(0.30);
        let t = HashSet::new();
        let scored = rerank_with_policy(vec![a, b], &t, NOW, 10, DecayPolicy::Strict);
        let ids: Vec<&str> = scored.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["b"], "low-salience hit must drop under Strict");
    }

    #[test]
    fn balanced_policy_drops_between_strict_and_loose() {
        let mut a = hit("g", "a", 0);
        a.salience = Some(0.10);
        let mut b = hit("g", "b", 1);
        b.salience = Some(0.17);
        let mut c = hit("g", "c", 2);
        c.salience = Some(0.30);
        let t = HashSet::new();
        let scored = rerank_with_policy(vec![a, b, c], &t, NOW, 10, DecayPolicy::Balanced);
        let ids: Vec<&str> = scored.iter().map(|s| s.id.as_str()).collect();
        // a (0.10) drops; b (0.17) survives (> 0.15); c (0.30) survives.
        // sorted by score desc — id alphabetical tie-break would be a/b/c
        // but a is gone; rel*salience: b=0.0028, c=0.0048 → c, b.
        assert_eq!(ids, vec!["c", "b"]);
    }

    #[test]
    fn loose_policy_drops_nothing() {
        let mut a = hit("g", "a", 0);
        a.salience = Some(0.01);
        let t = HashSet::new();
        let scored = rerank_with_policy(vec![a], &t, NOW, 10, DecayPolicy::Loose);
        assert_eq!(scored.len(), 1);
    }

    #[test]
    fn strip_memory_metas_removes_only_memory_signals() {
        let html = render_artifact(
            "demo",
            "<p>hello</p>",
            "memory-user",
            &["alpha".into(), "beta".into()],
            Some(0.7),
            Some("slow"),
            Some("abc123"),
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        // Sanity — fixture carries the metas we're about to strip.
        assert!(html.contains("kb-salience"));
        assert!(html.contains("kb-decay"));
        assert!(html.contains("kb-supersedes"));

        let stripped = strip_memory_metas(&html);
        assert!(!stripped.contains("kb-salience"));
        assert!(!stripped.contains("kb-decay"));
        assert!(!stripped.contains("kb-supersedes"));
        assert!(!stripped.contains("kb-pinned"));
        // kb-category + kb-tags survive — they're not memory-specific.
        assert!(stripped.contains("kb-category"));
        assert!(stripped.contains("kb-tags"));
        // Body + title untouched.
        assert!(stripped.contains("<h1>demo</h1>"));
        assert!(stripped.contains("<p>hello</p>"));
    }

    #[test]
    fn strip_memory_metas_is_idempotent() {
        let plain = render_artifact(
            "demo",
            "<p>hi</p>",
            "notes",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let once = strip_memory_metas(&plain);
        let twice = strip_memory_metas(&once);
        assert_eq!(once, twice);
        assert_eq!(once, plain, "no-op when no memory metas present");
    }

    #[test]
    fn pinned_memory_survives_strict_policy() {
        let mut a = hit("g", "a", 0);
        a.salience = Some(0.05);
        a.pinned = true;
        let t = HashSet::new();
        let scored = rerank_with_policy(vec![a], &t, NOW, 10, DecayPolicy::Strict);
        assert_eq!(scored.len(), 1, "pinned must survive Strict floor");
    }

    // invariant:10 rank-salience-decay
    #[test]
    fn rerank_is_deterministic() {
        let mk = || vec![hit("g", "a", 0), hit("g", "b", 1), hit("p", "c", 0)];
        let t = HashSet::new();
        assert_eq!(rerank(mk(), &t, NOW, 10), rerank(mk(), &t, NOW, 10));
    }

    // invariant:10 — the decomposition `rerank_with_policy` surfaces on
    // `Scored` (rank/rel/decay/age_days) must recompose EXACTLY into
    // `score`, and each factor must match the closed-form math directly
    // (never a re-derivation drifting from the scorer).
    #[test]
    fn scored_decomposition_recomposes_to_score() {
        let mut h = hit("g", "a", 3);
        h.salience = Some(0.42);
        h.decay = Some("fast".into());
        h.mtime_unix = Some(NOW - 10 * DAY);
        let out = rerank(vec![h], &HashSet::new(), NOW, 10);
        let s = &out[0];

        assert_eq!(s.rank, 3);
        assert!(
            (s.age_days - 10.0).abs() < 1e-6,
            "age_days = {}",
            s.age_days
        );
        let expected_rel = 1.0 / (60.0 + 3.0);
        assert!((s.rel - expected_rel).abs() < 1e-6, "rel = {}", s.rel);
        let expected_decay = (-DECAY_FAST * 10.0_f32).exp();
        assert!(
            (s.decay - expected_decay).abs() < 1e-6,
            "decay = {}",
            s.decay
        );
        let recomposed = s.rel * s.salience * s.decay;
        assert!(
            (s.score - recomposed).abs() < 1e-6,
            "score {} != rel*salience*decay {}",
            s.score,
            recomposed
        );
    }

    /// MI-W2.R (minor fix) — extend the decomposition-recomposition
    /// contract to BOTH v2 factors (relevance + stability, both flags on),
    /// held away from their trivial extremes: a mid-range, non-tied
    /// `relevance_factor` (not `None`, not exactly `0.0`/`1.0`) AND a
    /// non-zero `recall_count` (so `stability` isn't the never-recalled
    /// `1.0` no-op). `score` must still equal exactly
    /// `rel * salience * decay * relevance_factor`.
    #[test]
    fn scored_decomposition_recomposes_to_score_with_scoring_v2_mid_range_factors() {
        let mk = |id: &str, score: f32| {
            let mut h = hit("g", id, 3);
            h.salience = Some(0.42);
            h.decay = Some("fast".into());
            h.mtime_unix = Some(NOW - 10 * DAY);
            h.score = Some(score);
            h.recall_count = 4;
            h.last_recalled_at = Some(NOW - 5 * DAY);
            h
        };
        // Three engine scores in one corpus so the MIDDLE hit lands at a
        // genuinely mid-range (non-tied, non-0/1) relevance_factor.
        let out = rerank_with_policy_scored(
            vec![mk("lo", 1.0), mk("mid", 5.0), mk("hi", 9.0)],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            true,
        );
        let s = out.iter().find(|s| s.id == "mid").expect("mid hit present");

        let rf = s.relevance_factor.expect("scoring_v2 on ⇒ Some");
        assert!(
            (rf - 0.5).abs() < 1e-6,
            "mid-range relevance_factor should be 0.5, got {rf}"
        );
        let stability = s.stability.expect("scoring_v2 on ⇒ Some");
        assert!(
            stability > 1.0 && stability < STABILITY_CEILING,
            "recall_count > 0 must produce a non-trivial (non-extreme) stability, got {stability}"
        );

        let recomposed = s.rel * s.salience * s.decay * rf;
        assert!(
            (s.score - recomposed).abs() < 1e-6,
            "score {} != rel*salience*decay*relevance_factor {}",
            s.score,
            recomposed
        );
    }

    // === MI-W4.1 — `Scored::decay_k` =======================================

    #[test]
    fn scored_decay_k_matches_the_bucket_the_hit_carried() {
        let mut fast = hit("g", "f", 0);
        fast.decay = Some("fast".into());
        let mut slow = hit("g", "s", 0);
        slow.decay = Some("slow".into());
        let mut unset = hit("g", "u", 0);
        unset.decay = None;
        let out = rerank(vec![fast, slow, unset], &HashSet::new(), NOW, 10);
        let by_id = |id: &str| out.iter().find(|s| s.id == id).unwrap();
        assert_eq!(by_id("f").decay_k, DECAY_FAST);
        assert_eq!(by_id("s").decay_k, DECAY_SLOW);
        // Absent/unknown bucket defaults to slow, matching `decay_k`'s own
        // fallback — the two can never disagree since `decay_k` is the
        // ONE function both `rerank_with_policy_scored` and this field
        // read.
        assert_eq!(by_id("u").decay_k, DECAY_SLOW);
    }

    // === MI-W4.1(revision) — `decay_half_life_days` / `FloorState` ========
    //
    // These pin the TRUE semantics the previous `days_until_floor_crossing`
    // helper got wrong: a high-salience memory can NEVER be predicted to
    // drop no matter how old (ground truth — the floor tests raw salience,
    // never a decayed value), and a below-floor memory is reported as
    // excluded NOW, not on some future date.

    #[test]
    fn decay_half_life_matches_the_two_named_buckets() {
        // ln(2)/0.01 ≈ 69.3d (slow), ln(2)/0.1 ≈ 6.93d (fast) — the exact
        // numbers the design brief's "score halves every ~69d / ~7d"
        // phrasing is built from.
        assert!((decay_half_life_days(DECAY_SLOW) - 69.314_72).abs() < 1e-2);
        assert!((decay_half_life_days(DECAY_FAST) - 6.931_472).abs() < 1e-3);
    }

    #[test]
    fn decay_half_life_is_infinite_for_a_non_positive_rate() {
        assert!(decay_half_life_days(0.0).is_infinite());
        assert!(decay_half_life_days(-1.0).is_infinite());
    }

    #[test]
    fn decay_half_life_is_independent_of_stability_and_age() {
        // The half-life is a property of `decay_k` alone — it does NOT
        // take salience/age/stability, unlike the deleted crossing helper.
        // This test exists to pin the SIGNATURE contract: the function
        // compiles with exactly one argument.
        let h1 = decay_half_life_days(DECAY_SLOW);
        let h2 = decay_half_life_days(DECAY_SLOW);
        assert_eq!(h1, h2);
    }

    #[test]
    fn floor_state_pinned_short_circuits_before_any_salience_comparison() {
        // Even a salience of 0.0 against a high floor must report Pinned,
        // never Below — pinned is checked FIRST.
        assert_eq!(floor_state(0.0, Some(0.9), true), FloorState::Pinned);
    }

    #[test]
    fn floor_state_no_floor_for_the_loose_policy() {
        assert_eq!(floor_state(0.01, None, false), FloorState::NoFloor);
    }

    #[test]
    fn floor_state_above_when_salience_exceeds_the_floor() {
        assert_eq!(
            floor_state(0.40, Some(0.15), false),
            FloorState::Above {
                salience: 0.40,
                floor: 0.15
            }
        );
    }

    #[test]
    fn floor_state_below_when_salience_is_at_or_under_the_floor() {
        assert_eq!(
            floor_state(0.15, Some(0.15), false),
            FloorState::Below {
                salience: 0.15,
                floor: 0.15
            },
            "AT the floor counts as below — matches the `<=` filter rerank_with_policy_scored applies"
        );
        assert_eq!(
            floor_state(0.10, Some(0.15), false),
            FloorState::Below {
                salience: 0.10,
                floor: 0.15
            }
        );
    }

    /// THE key correctness property this revision exists to establish: a
    /// high-salience memory's `FloorState` does not depend on age at
    /// all — there is no age parameter to pass. A caller cannot even
    /// construct the false "ancient high-salience memory is about to drop"
    /// claim the deleted `days_until_floor_crossing` allowed, because
    /// `floor_state` never accepts an age/decay-rate/stability input in the
    /// first place.
    #[test]
    fn floor_state_never_predicts_a_drop_for_high_salience_regardless_of_any_notion_of_age() {
        // Same salience, same floor — the state is identical no matter how
        // many times or in what "temporal" framing a caller might imagine
        // re-evaluating it, because the function is pure over
        // (salience, floor, pinned) alone.
        let a = floor_state(0.9, Some(0.15), false);
        let b = floor_state(0.9, Some(0.15), false);
        assert_eq!(a, b);
        assert!(matches!(a, FloorState::Above { .. }));
    }

    #[test]
    fn higher_salience_outranks_at_same_rank() {
        let mut lo = hit("g", "lo", 0);
        lo.salience = Some(0.2);
        let mut hi = hit("g", "hi", 0);
        hi.salience = Some(0.9);
        let out = rerank(vec![lo, hi], &HashSet::new(), NOW, 10);
        assert_eq!(out[0].id, "hi");
        assert_eq!(out[1].id, "lo");
    }

    #[test]
    fn older_memory_decays_below_newer_at_same_rank() {
        let mut newish = hit("g", "new", 0);
        newish.mtime_unix = Some(NOW - DAY);
        newish.decay = Some("fast".into());
        let mut old = hit("g", "old", 0);
        old.mtime_unix = Some(NOW - 100 * DAY);
        old.decay = Some("fast".into());
        let out = rerank(vec![old, newish], &HashSet::new(), NOW, 10);
        assert_eq!(out[0].id, "new");
    }

    #[test]
    fn salience_defaults_to_half_when_absent() {
        let out = rerank(vec![hit("g", "a", 0)], &HashSet::new(), NOW, 10);
        assert_eq!(out[0].salience, 0.5);
    }

    // invariant:10 supersede-drop
    #[test]
    fn superseded_id_is_dropped() {
        let tomb: HashSet<String> = ["old".to_string()].into_iter().collect();
        let out = rerank(
            vec![hit("g", "old", 0), hit("g", "keep", 1)],
            &tomb,
            NOW,
            10,
        );
        let ids: Vec<_> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["keep"]);
    }

    #[test]
    fn forgotten_status_is_dropped() {
        let mut gone = hit("g", "gone", 0);
        gone.status = Some("forgotten".into());
        let out = rerank(vec![gone, hit("g", "keep", 1)], &HashSet::new(), NOW, 10);
        let ids: Vec<_> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["keep"]);
    }

    #[test]
    fn dedups_by_id_keeping_best_score() {
        // Same id from two corpora at different ranks; the rank-0 hit
        // scores higher, so it wins and the duplicate is dropped.
        let out = rerank(
            vec![hit("g", "x", 5), hit("p", "x", 0)],
            &HashSet::new(),
            NOW,
            10,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kb, "p");
    }

    #[test]
    fn truncates_to_limit() {
        let hits: Vec<_> = (0..10).map(|i| hit("g", &format!("h{i}"), i)).collect();
        assert_eq!(rerank(hits, &HashSet::new(), NOW, 3).len(), 3);
    }

    #[test]
    fn unknown_or_absent_decay_treated_as_slow() {
        assert_eq!(decay_k(Some("weird")), DECAY_SLOW);
        assert_eq!(decay_k(None), DECAY_SLOW);
        assert_eq!(decay_k(Some("fast")), DECAY_FAST);
    }

    // invariant:10 kb-created
    #[test]
    fn render_artifact_round_trips_through_parser() {
        let html = render_artifact(
            "User prefers tabs",
            &text_to_body_html("They said tabs beat spaces."),
            "memory-user",
            &["preferences".to_string(), "style".to_string()],
            Some(0.5),
            Some("slow"),
            Some("abc123def456"),
            Some("sess-2026-01-01"),
            true,
            &["kb-alpha".to_string(), "kb-beta".to_string()],
            Some(1_700_000_000),
            Some("Tabs over spaces, always"),
            None,
            Some(MemoryType::Semantic),
            None,
        );
        let f = crate::parser::extract(&html);
        assert_eq!(f.title.as_deref(), Some("User prefers tabs"));
        assert_eq!(f.kb_category.as_deref(), Some("memory-user"));
        assert_eq!(f.kb_salience, Some(0.5));
        assert_eq!(f.kb_decay.as_deref(), Some("slow"));
        assert_eq!(f.kb_supersedes.as_deref(), Some("abc123def456"));
        assert_eq!(f.kb_session.as_deref(), Some("sess-2026-01-01"));
        // RA3 — kb-created round-trips as the decay basis.
        assert_eq!(f.kb_created, Some(1_700_000_000));
        // RA4 — kb-summary round-trips as the one-line gloss.
        assert_eq!(f.kb_summary.as_deref(), Some("Tabs over spaces, always"));
        assert!(f.kb_global);
        assert_eq!(f.kb_linked_kbs, vec!["kb-alpha", "kb-beta"]);
        assert!(f.tags.contains(&"preferences".to_string()));
        assert!(f.body.contains("tabs"));
        // MI-W3.3a — kb-memory-type round-trips.
        assert_eq!(f.kb_memory_type.as_deref(), Some("semantic"));
    }

    #[test]
    fn render_artifact_escapes_title_and_omits_absent_metas() {
        let html = render_artifact(
            "a < b & c",
            "<p>x</p>",
            "memory-project",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert!(html.contains("a &lt; b &amp; c"));
        assert!(!html.contains("kb-salience"));
        assert!(!html.contains("kb-decay"));
        assert!(!html.contains("kb-supersedes"));
        assert!(!html.contains("kb-session"));
        assert!(!html.contains("kb-tags"));
        assert!(!html.contains("kb-global"));
        assert!(!html.contains("kb-linked-kbs"));
        assert!(!html.contains("kb-created"));
        assert!(!html.contains("kb-summary"));
        assert!(!html.contains("kb-memory-type"));
        assert!(!html.contains("kb-source"));
        // U3 — no provenance record ⇒ not one provenance meta.
        assert!(!html.contains("kb-author"));
        assert!(!html.contains("kb-source-kb"));
        assert!(!html.contains("kb-source-artifact"));
        assert!(!html.contains("kb-source-anchor"));
    }

    #[test]
    fn text_to_body_html_splits_paragraphs_and_escapes() {
        let out = text_to_body_html("first <para>\n\nsecond");
        assert!(out.contains("<p>first &lt;para&gt;</p>"));
        assert!(out.contains("<p>second</p>"));
    }

    #[test]
    fn memory_slug_is_filename_safe() {
        assert_eq!(memory_slug("Hello, World!"), "hello-world");
        assert_eq!(memory_slug("  !!!  "), "memory");
        assert_eq!(memory_slug("Already-Slugged_v2"), "already-slugged-v2");
    }

    // ---- U3 — highlight provenance ------------------------------------

    fn highlight_provenance() -> MemoryProvenance {
        MemoryProvenance {
            author: Some(Author::You),
            source_kb: Some("kb-docs".into()),
            source_artifact: Some("a1b2c3d4e5f6".into()),
            source_anchor: Some(Anchor::Selection {
                css_path: "main > p:nth-of-type(2)".into(),
                offset: 17,
                snippet: "a \"quoted\" & <angled> phrase".into(),
            }),
            // MI-W3.4 — rides the SAME fixture so `provenance_cannot_reach_
            // the_scorer` exercises kb-source through the identical path
            // as author/source_kb/source_artifact/source_anchor.
            source: Some(TrustSource::FetchedWeb),
        }
    }

    fn render_with(prov: Option<&MemoryProvenance>) -> String {
        render_artifact(
            "Highlighted claim",
            &text_to_body_html("the selection, verbatim"),
            "memory-user",
            &["reading".to_string()],
            Some(DEFAULT_SALIENCE),
            Some("slow"),
            None,
            None,
            true,
            &[],
            Some(NOW),
            None,
            prov,
            None,
            None,
        )
    }

    #[test]
    fn render_artifact_records_highlight_provenance() {
        let html = render_with(Some(&highlight_provenance()));
        assert!(html.contains(r#"<meta name="kb-author" content="you">"#));
        assert!(html.contains(r#"<meta name="kb-source-kb" content="kb-docs">"#));
        assert!(html.contains(r#"<meta name="kb-source-artifact" content="a1b2c3d4e5f6">"#));
        // The anchor rides `lists::anchor_to_json` (#25 — one anchor JSON
        // grammar), attribute-escaped into the meta.
        let json = crate::lists::anchor_to_json(&Anchor::Selection {
            css_path: "main > p:nth-of-type(2)".into(),
            offset: 17,
            snippet: "a \"quoted\" & <angled> phrase".into(),
        });
        assert!(html.contains(&format!(
            r#"<meta name="kb-source-anchor" content="{}">"#,
            escape_attr(&json)
        )));
        // MI-W3.4 — the trust tag rides alongside the other provenance metas.
        assert!(html.contains(r#"<meta name="kb-source" content="fetched-web">"#));
        // Escaped, not raw: a snippet's quotes/angles can never break out
        // of the attribute (the meta is machine-written but the snippet is
        // arbitrary artifact text).
        assert!(!html.contains(r#"a "quoted" & <angled>"#));
        // The BODY is the selection verbatim — provenance adds no prose,
        // no generated summary, no footer (ruling 4: never a summary).
        let body = html.split("<main>").nth(1).unwrap();
        assert_eq!(
            body.trim(),
            "<h1>Highlighted claim</h1>\n<p>the selection, verbatim</p>\n</main>\n</body>\n</html>",
        );
    }

    #[test]
    fn empty_provenance_renders_byte_identically_to_none() {
        let empty = MemoryProvenance::default();
        assert!(empty.is_empty());
        assert_eq!(render_with(Some(&empty)), render_with(None));
    }

    #[test]
    fn author_meta_matches_the_review_wire_form() {
        // The `you | claude` ROLE split, spelled exactly as a review file
        // spells it — one vocabulary, not a second one.
        assert_eq!(author_meta(Author::You), "you");
        assert_eq!(author_meta(Author::Claude), "claude");
        assert_eq!(
            serde_json::to_string(&Author::You).unwrap(),
            format!("\"{}\"", author_meta(Author::You))
        );
    }

    // invariant:10 — provenance is a SURFACED signal, NEVER a score term.
    // Two ways it could leak into ranking, both pinned here:
    //   1. through the parsed scoring inputs (salience/decay/created), and
    //   2. through `rerank`'s ARITHMETIC — `RecallHit`/`Scored` DO carry
    //      `author`/`source_kb`/`source_artifact`/`source_anchor` (CT-A1,
    //      U3 parse-back — pure pass-through display fields, exactly like
    //      `memory_type`/`source`), but the scoring formula in
    //      `rerank_with_policy_scored` never reads any of them; this test
    //      is the reason it must never start to.
    //
    // MI-W3.4 extends this explicitly to the write-time TRUST tag
    // (`kb-source`), and CT-A1 extends it again to `author`/`source_kb`/
    // `source_artifact`/`source_anchor` (now ALSO parsed into `Fields` and
    // projected onto `RecallHit`/`Scored` — no longer write-only) — the
    // `with`/`without` fixture below differs in ALL FIVE provenance metas
    // (`highlight_provenance()` sets every field; the `without` fixture has
    // no provenance at all), so any of the scoring-input assertions failing
    // would mean one of them leaked into the parsed scoring inputs.
    #[test]
    fn provenance_cannot_reach_the_scorer() {
        let with = crate::parser::extract(&render_with(Some(&highlight_provenance())));
        let without = crate::parser::extract(&render_with(None));
        assert_eq!(with.kb_salience, without.kb_salience);
        assert_eq!(with.kb_decay, without.kb_decay);
        assert_eq!(with.kb_created, without.kb_created);
        assert_eq!(with.kb_category, without.kb_category);
        assert_eq!(with.tags, without.tags);
        assert_eq!(with.body, without.body);
        // Sanity — every provenance meta really is present on one side and
        // absent on the other, so the assertions above are testing
        // something.
        assert_eq!(with.kb_source.as_deref(), Some("fetched-web"));
        assert_eq!(without.kb_source, None);
        assert_eq!(with.kb_author.as_deref(), Some("you"));
        assert_eq!(without.kb_author, None);
        assert_eq!(with.kb_source_kb.as_deref(), Some("kb-docs"));
        assert_eq!(without.kb_source_kb, None);
        assert_eq!(with.kb_source_artifact.as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(without.kb_source_artifact, None);
        assert!(with.kb_source_anchor.is_some());
        assert_eq!(without.kb_source_anchor, None);

        // And the ordering itself: a human-authored memory and an agent-
        // authored one at the same rank/salience/age tie EXACTLY (score
        // equal, id asc tie-break), so provenance buys no boost and pays
        // no penalty. The two hits differ in EVERY provenance field
        // (author/source_kb/source_artifact/source_anchor) — if any of them
        // reached the formula, the scores would diverge.
        let mk = |id: &str, rank: usize, author: &str| {
            let mut h = hit("g", id, rank);
            h.salience = Some(with.kb_salience.unwrap_or(DEFAULT_SALIENCE));
            h.decay = with.kb_decay.clone();
            h.mtime_unix = Some(NOW - DAY);
            h.author = Some(author.to_string());
            h.source_kb = Some(format!("kb-{author}"));
            h.source_artifact = Some(format!("{author}deadbeef01"));
            h.source_anchor = Some(format!(r#"{{"kind":"file","who":"{author}"}}"#));
            h
        };
        let out = rerank(
            vec![
                mk("human-highlight", 0, "you"),
                mk("agent-remember", 0, "claude"),
            ],
            &HashSet::new(),
            NOW,
            10,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].score, out[1].score,
            "provenance must not tilt the score"
        );
        let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["agent-remember", "human-highlight"], "id asc");
    }

    // MI-W3.R — `provenance_cannot_reach_the_scorer` got the kb-source
    // (MI-W3.4) two-layer isolation check (parser fields + rerank tie-break)
    // but MI-W3.3a's `kb-memory-type` never got the sibling treatment. Same
    // shape as the kb_source assertions above: a `with`/`without` fixture
    // differing ONLY in `memory_type` (a top-level `render_artifact` param,
    // not part of `MemoryProvenance`, so it needs its own render call rather
    // than reusing `render_with`), then the same rerank tie-break — two
    // hits differing ONLY in `memory_type` must score EXACTLY equally and
    // tie-break identically (id asc).
    #[test]
    fn memory_type_cannot_reach_the_scorer() {
        let with = crate::parser::extract(&render_artifact(
            "Highlighted claim",
            &text_to_body_html("the selection, verbatim"),
            "memory-user",
            &["reading".to_string()],
            Some(DEFAULT_SALIENCE),
            Some("slow"),
            None,
            None,
            true,
            &[],
            Some(NOW),
            None,
            None,
            Some(MemoryType::Semantic),
            None,
        ));
        let without = crate::parser::extract(&render_with(None));
        assert_eq!(with.kb_salience, without.kb_salience);
        assert_eq!(with.kb_decay, without.kb_decay);
        assert_eq!(with.kb_created, without.kb_created);
        assert_eq!(with.kb_category, without.kb_category);
        assert_eq!(with.tags, without.tags);
        assert_eq!(with.body, without.body);
        // Sanity — the type really is present on one side and absent on
        // the other, so the assertions above are testing something.
        assert_eq!(with.kb_memory_type.as_deref(), Some("semantic"));
        assert_eq!(without.kb_memory_type, None);

        // Same tie-break shape as the kb_source check: two hits differing
        // ONLY in `memory_type` tie EXACTLY.
        let mk = |id: &str, memory_type: Option<&str>| {
            let mut h = hit("g", id, 0);
            h.salience = Some(with.kb_salience.unwrap_or(DEFAULT_SALIENCE));
            h.decay = with.kb_decay.clone();
            h.mtime_unix = Some(NOW - DAY);
            h.memory_type = memory_type.map(String::from);
            h
        };
        let out = rerank(
            vec![mk("typed-semantic", Some("semantic")), mk("untyped", None)],
            &HashSet::new(),
            NOW,
            10,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].score, out[1].score,
            "memory_type must not tilt the score"
        );
        let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["typed-semantic", "untyped"], "id asc");
    }

    // ==== MI-W2.1 — relevance term, flag-gated ============================

    /// MI-W5.R re-anchor — this pins **v1 behaviour when BOTH v2 flags are
    /// off** (NOT the shipped default anymore, now that
    /// `scoring_v2_relevance` defaults to `true`; see
    /// `shipped_default_flags_are_relevance_on_stability_off` below for the
    /// default combination). Recomputed here by hand (not by diffing two
    /// live implementations), mirroring
    /// `scored_decomposition_recomposes_to_score`'s fixture.
    #[test]
    fn both_scoring_v2_flags_off_matches_the_pre_change_formula() {
        let mut h = hit("g", "a", 3);
        h.salience = Some(0.42);
        h.decay = Some("fast".into());
        h.mtime_unix = Some(NOW - 10 * DAY);
        h.score = Some(0.9); // present but must be ignored when off
        h.recall_count = 7; // present but must be ignored when off

        let out = rerank_with_policy_scored(
            vec![h.clone()],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            false,
        );
        let s = &out[0];
        assert!(s.relevance_factor.is_none());
        assert!(s.stability.is_none());

        let expected_rel = 1.0 / (60.0 + 3.0);
        let expected_decay = (-DECAY_FAST * 10.0_f32).exp();
        let expected_score = expected_rel * 0.42 * expected_decay;
        assert!((s.rel - expected_rel).abs() < 1e-6);
        assert!((s.decay - expected_decay).abs() < 1e-6);
        assert!((s.score - expected_score).abs() < 1e-6);

        // And it's identical to the public wrapper, which delegates with
        // both flags `false` — pinning the delegation itself, not just the
        // math.
        let via_wrapper =
            rerank_with_policy(vec![h], &HashSet::new(), NOW, 10, DecayPolicy::Balanced);
        assert_eq!(out, via_wrapper);
    }

    /// MI-W5.R — pins the SHIPPED DEFAULT combination
    /// (`scoring_v2_relevance = true`, `scoring_v2_stability = false`,
    /// [`crate::config::MemorySection::default`]): relevance is applied
    /// (measured on the live corpus, W5.1 bench), stability is not
    /// (unmeasured — the bench never loaded the sessions corpus the
    /// `memory_recalls` ledger lives in). A high `recall_count` is set
    /// specifically to prove stability stays a no-op (`None`, not just a
    /// neutral `1.0`) even though the ledger data is present — only the
    /// flag decides.
    #[test]
    fn shipped_default_flags_are_relevance_on_stability_off() {
        let cfg = crate::config::MemorySection::default();
        assert!(cfg.scoring_v2_relevance, "shipped default: relevance ON");
        assert!(!cfg.scoring_v2_stability, "shipped default: stability OFF");

        let mut h = hit("g", "a", 3);
        h.salience = Some(0.5);
        h.decay = Some("fast".into());
        h.mtime_unix = Some(NOW - DAY);
        h.score = Some(0.7);
        h.recall_count = 50; // would move stability if the flag were on
        h.last_recalled_at = Some(NOW - DAY);

        let out = rerank_with_policy_scored(
            vec![h],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            cfg.scoring_v2_relevance,
            cfg.scoring_v2_stability,
        );
        let s = &out[0];
        assert!(
            s.relevance_factor.is_some(),
            "shipped default must apply the relevance factor"
        );
        assert!(
            s.stability.is_none(),
            "shipped default must NOT apply the stability factor, regardless of ledger data"
        );
    }

    /// A hit with `score: None` (the empty-query/list_docs timeline path)
    /// must score BYTE-IDENTICALLY whether BOTH v2 flags are on or off — the
    /// relevance factor degrades to a neutral 1.0 (never multiplies the
    /// score at all when off; multiplies by exactly 1.0, bit-exact, when
    /// on), and with `recall_count == 0` the stability factor is likewise
    /// an exact no-op.
    #[test]
    fn scoring_v2_on_with_no_score_is_byte_identical_to_off() {
        let mk = || {
            let mut h = hit("g", "a", 1);
            h.salience = Some(0.6);
            h.decay = Some("slow".into());
            h.mtime_unix = Some(NOW - 5 * DAY);
            h.score = None; // list_docs path
            h
        };
        let off = rerank_with_policy_scored(
            vec![mk()],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            false,
        );
        let on = rerank_with_policy_scored(
            vec![mk()],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            true,
        );
        assert_eq!(off[0].score, on[0].score, "score must be bit-identical");
        assert_eq!(on[0].relevance_factor, Some(1.0));
        assert_eq!(on[0].stability, Some(1.0));
    }

    #[test]
    fn compute_relevance_factors_degrades_gracefully() {
        // Single scored hit in its corpus → nothing to normalize against.
        let mut a = hit("g", "a", 0);
        a.score = Some(0.5);
        assert_eq!(compute_relevance_factors(&[a.clone()]), vec![1.0]);

        // Two hits, identical score → a tie, still neutral.
        let mut b = a.clone();
        b.id = "b".into();
        assert_eq!(
            compute_relevance_factors(&[a.clone(), b.clone()]),
            vec![1.0, 1.0]
        );

        // No score at all → neutral, regardless of how many siblings.
        let mut c = hit("g", "c", 0);
        c.score = None;
        let mut d = hit("g", "d", 0);
        d.score = None;
        assert_eq!(compute_relevance_factors(&[c, d]), vec![1.0, 1.0]);

        // A real spread normalizes to [0,1], min→0.0, max→1.0.
        let mut lo = hit("g", "lo", 0);
        lo.score = Some(1.0);
        let mut mid = hit("g", "mid", 0);
        mid.score = Some(2.0);
        let mut hi = hit("g", "hi", 0);
        hi.score = Some(3.0);
        let factors = compute_relevance_factors(&[lo, mid, hi]);
        assert!((factors[0] - 0.0).abs() < 1e-6);
        assert!((factors[1] - 0.5).abs() < 1e-6);
        assert!((factors[2] - 1.0).abs() < 1e-6);

        // Per-corpus: a spread in kb "p" must not leak into kb "g"'s
        // single-hit (degenerate) factor.
        let mut g_only = hit("g", "g1", 0);
        g_only.score = Some(9.0);
        let mut p_lo = hit("p", "p1", 0);
        p_lo.score = Some(0.0);
        let mut p_hi = hit("p", "p2", 0);
        p_hi.score = Some(10.0);
        let mixed = compute_relevance_factors(&[g_only, p_lo, p_hi]);
        assert_eq!(mixed[0], 1.0, "g's lone hit stays neutral");
        assert!((mixed[1] - 0.0).abs() < 1e-6);
        assert!((mixed[2] - 1.0).abs() < 1e-6);
    }

    /// End-to-end: with `scoring_v2_relevance` on (stability left off — this
    /// fixture isolates the relevance factor), a higher raw engine score at
    /// the SAME rank/salience/decay outranks a lower one — the relevance
    /// factor is doing real work, not just being surfaced inertly.
    #[test]
    fn relevance_factor_breaks_ties_by_engine_score_when_on() {
        let mk = |id: &str, score: f32| {
            let mut h = hit("g", id, 0);
            h.salience = Some(0.5);
            h.mtime_unix = Some(NOW - DAY);
            h.score = Some(score);
            h
        };
        let out = rerank_with_policy_scored(
            vec![mk("weak", 1.0), mk("strong", 5.0)],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            false,
        );
        assert_eq!(out[0].id, "strong");
        assert_eq!(out[0].relevance_factor, Some(1.0));
        assert_eq!(out[1].id, "weak");
        assert_eq!(out[1].relevance_factor, Some(0.0));
        assert_eq!(
            out[1].score, 0.0,
            "the corpus-worst engine score zeroes the term"
        );

        // Flag off → the two hits tie (rel/salience/decay identical), so
        // the id-ascending tie-break decides order instead.
        let off = rerank_with_policy_scored(
            vec![mk("weak", 1.0), mk("strong", 5.0)],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            false,
        );
        assert_eq!(off[0].id, "strong", "id asc tie-break: strong < weak");
        assert_eq!(off[0].score, off[1].score);
    }

    /// MI-W2.R (review fix, regression) — a forgotten/tombstoned sibling
    /// with an EXTREME raw engine score must not skew the survivor's
    /// `relevance_factor`. Before the fix, `compute_relevance_factors` ran
    /// over the unfiltered `hits` slice, so a doomed sibling's score still
    /// entered its corpus's min/max — this pins the survivor's factor to
    /// be byte-identical to a world where the sibling was never indexed at
    /// all, across BOTH drop reasons (`status: forgotten` and an explicit
    /// tombstone id) and across two corpora, so a corpus untouched by
    /// either drop reason is unaffected too.
    #[test]
    fn relevance_normalization_ignores_filtered_out_siblings() {
        // Corpus "g": one live hit + a FORGOTTEN sibling with a wildly
        // higher raw score than any live hit could plausibly have.
        let mut live_g = hit("g", "live-g", 0);
        live_g.salience = Some(0.5);
        live_g.mtime_unix = Some(NOW - DAY);
        live_g.score = Some(1.0);

        let mut forgotten_sibling = hit("g", "forgotten-g", 1);
        forgotten_sibling.status = Some("forgotten".into());
        forgotten_sibling.score = Some(1_000.0); // would dominate min-max if it leaked in

        // Corpus "p": one live hit + a TOMBSTONED sibling (dropped via the
        // `tombstones` set, not `status`), same extreme-score shape.
        let mut live_p = hit("p", "live-p", 0);
        live_p.salience = Some(0.5);
        live_p.mtime_unix = Some(NOW - DAY);
        live_p.score = Some(2.0);

        let mut tombstoned_sibling = hit("p", "tombstoned-p", 1);
        tombstoned_sibling.score = Some(-1_000.0); // extreme the OTHER direction
        let tombstones: HashSet<String> = ["tombstoned-p".to_string()].into_iter().collect();

        let with_siblings = rerank_with_policy_scored(
            vec![
                live_g.clone(),
                forgotten_sibling,
                live_p.clone(),
                tombstoned_sibling,
            ],
            &tombstones,
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            false,
        );

        // Baseline: as if the doomed siblings had never been indexed at all.
        let without_siblings = rerank_with_policy_scored(
            vec![live_g, live_p],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            false,
        );

        assert_eq!(with_siblings.len(), 2, "both siblings must be dropped");
        assert_eq!(without_siblings.len(), 2);

        let find = |v: &[Scored], id: &str| v.iter().find(|s| s.id == id).unwrap().clone();
        let g_with = find(&with_siblings, "live-g");
        let g_without = find(&without_siblings, "live-g");
        let p_with = find(&with_siblings, "live-p");
        let p_without = find(&without_siblings, "live-p");

        assert_eq!(
            g_with.relevance_factor, g_without.relevance_factor,
            "the forgotten sibling's extreme score must not skew live-g's normalization"
        );
        assert_eq!(
            p_with.relevance_factor, p_without.relevance_factor,
            "the tombstoned sibling's extreme score must not skew live-p's normalization"
        );
        // Each corpus is now its own lone scored survivor → neutral 1.0,
        // not some fraction of a [-1000, 1000]-ish spread.
        assert_eq!(g_with.relevance_factor, Some(1.0));
        assert_eq!(p_with.relevance_factor, Some(1.0));
        assert_eq!(g_with.score, g_without.score);
        assert_eq!(p_with.score, p_without.score);
    }

    // ==== MI-W2.2 — stability term over the ledger =========================

    #[test]
    fn never_recalled_stability_is_exactly_one() {
        for (created, last, salience, bucket) in [
            (None, None, 0.5, None),
            (Some(NOW - 100 * DAY), None, 0.9, Some("fast")),
            (
                Some(NOW - 100 * DAY),
                Some(NOW - 50 * DAY),
                0.1,
                Some("slow"),
            ),
        ] {
            assert_eq!(
                stability_multiplier(0, last, created, salience, bucket),
                1.0
            );
        }
    }

    /// Table of (inputs → expected ordering), per the unit's requirement:
    /// holding every other input fixed, MORE recalls never yields a LOWER
    /// stability.
    #[test]
    fn stability_is_monotonic_in_recall_count() {
        let created = Some(NOW - 200 * DAY);
        let last = Some(NOW - 100 * DAY); // recalled well before "now" — some difficulty
        let salience = 0.3;
        let bucket = Some("fast");
        let counts = [0u32, 1, 2, 5, 20, 1_000, 1_000_000];
        let values: Vec<f32> = counts
            .iter()
            .map(|&n| stability_multiplier(n, last, created, salience, bucket))
            .collect();
        for w in values.windows(2) {
            assert!(
                w[1] >= w[0] - 1e-6,
                "stability must never decrease as recall_count grows: {values:?}"
            );
        }
        // And it actually MOVES (not a flat no-op curve) somewhere in the
        // table — otherwise "monotonic" would be vacuously true.
        assert!(values[0] < values[values.len() - 1]);
    }

    #[test]
    fn stability_is_bounded_by_the_ceiling() {
        let s = stability_multiplier(
            u32::MAX,
            Some(NOW - 100 * DAY),
            Some(NOW - 200 * DAY),
            0.0,
            Some("fast"),
        );
        assert!(s.is_finite());
        assert_eq!(s, STABILITY_CEILING);
    }

    /// A finite stability ceiling can slow decay but can never halt it: an
    /// extremely old memory with the maximum possible stability still
    /// decays to near-zero, never "immortal" (decay pinned near 1.0
    /// forever).
    #[test]
    fn stability_never_makes_a_memory_immortal() {
        let k = DECAY_FAST;
        let age_days = 100_000.0_f32; // absurdly old
        let base_decay = (-k * age_days).exp();
        let s = stability_multiplier(
            u32::MAX,
            Some(NOW - 90_000 * DAY),
            Some(NOW - 100_000 * DAY),
            0.0,
            Some("fast"),
        );
        assert_eq!(s, STABILITY_CEILING, "sanity: the ceiling clamp engaged");
        let decay_v2 = (base_decay * s).min(1.0);
        assert!(
            decay_v2 < 1e-6,
            "an ancient, maximally-stabilized memory must still be ~fully decayed: {decay_v2}"
        );
    }

    /// End-to-end ordering table: with `scoring_v2_stability` on (relevance
    /// left off — none of these hits carry a `score`, and this fixture
    /// isolates the stability factor anyway), holding rank/salience/decay/
    /// age fixed, a memory recalled more often (and closer to its decay
    /// floor when it was) outranks one recalled less — via decay alone, no
    /// relevance-score signal involved.
    #[test]
    fn stability_end_to_end_ordering_table() {
        let mk = |id: &str, recall_count: u32, last_recalled_at: Option<i64>| {
            let mut h = hit("g", id, 0);
            h.salience = Some(0.5);
            h.decay = Some("fast".into());
            h.mtime_unix = Some(NOW - 200 * DAY); // created — same age for all
            h.recall_count = recall_count;
            h.last_recalled_at = last_recalled_at;
            h
        };
        let never = mk("never", 0, None);
        let recalled_once_fresh = mk("once-fresh", 1, Some(NOW - 199 * DAY)); // recalled right after creation
        let recalled_often_stale = mk("often-stale", 20, Some(NOW - 10 * DAY)); // recalled near-fully-decayed, many times

        let out = rerank_with_policy_scored(
            vec![never, recalled_once_fresh, recalled_often_stale],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            true,
        );
        let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["often-stale", "once-fresh", "never"],
            "more + more-difficult recalls outrank fewer/easier outrank none"
        );

        // With the flag off, recall history is invisible — all three tie
        // (same rank/salience/decay/age) and fall back to the id
        // asc tie-break.
        let never = mk("never", 0, None);
        let recalled_once_fresh = mk("once-fresh", 1, Some(NOW - 199 * DAY));
        let recalled_often_stale = mk("often-stale", 20, Some(NOW - 10 * DAY));
        let off = rerank_with_policy_scored(
            vec![never, recalled_once_fresh, recalled_often_stale],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            false,
        );
        let off_ids: Vec<&str> = off.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            off_ids,
            vec!["never", "often-stale", "once-fresh"],
            "id asc"
        );
    }

    // ==== MI-W3.2b — salience edit ==========================================

    #[test]
    fn set_salience_html_splices_and_round_trips() {
        let html = render_artifact(
            "demo",
            "<p>hello</p>",
            "memory-user",
            &[],
            Some(0.2),
            Some("slow"),
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let out = set_salience(&html, false, 0.85);
        assert!(out.contains(r#"<meta name="kb-salience" content="0.85">"#));
        let f = crate::parser::extract(&out);
        assert_eq!(f.kb_salience, Some(0.85));
        // Untouched: every other meta + the body survive verbatim.
        assert!(out.contains("kb-decay"));
        assert!(out.contains("<h1>demo</h1>"));
        assert!(out.contains("<p>hello</p>"));
    }

    #[test]
    fn set_salience_html_from_absent_meta() {
        let html = render_artifact(
            "demo",
            "<p>x</p>",
            "memory-user",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        assert!(
            !html.contains("kb-salience"),
            "fixture sanity: absent to start"
        );
        let out = set_salience(&html, false, 0.4);
        let f = crate::parser::extract(&out);
        assert_eq!(f.kb_salience, Some(0.4));
    }

    #[test]
    fn set_salience_markdown_writes_frontmatter() {
        let md = "---\nkb-category: memory-user\nkb-salience: 0.3\n---\n\n# demo\n\nhello\n";
        let out = set_salience(md, true, 0.9);
        assert!(out.contains("kb-salience: 0.9"));
        assert!(
            !out.contains("kb-salience: 0.3"),
            "old value replaced, not duplicated"
        );
        assert!(
            out.contains("kb-category: memory-user"),
            "sibling frontmatter survives"
        );
        assert!(out.contains("hello"), "body untouched");
    }

    #[test]
    fn set_salience_reindex_stability_reparses_to_the_exact_value() {
        // MI-W3.2b requirement: round-trip + reindex stability — writing,
        // then re-parsing via the SAME `parser::extract` the indexer calls
        // on every reindex, must yield exactly the written value, repeatedly
        // (a second reindex of an unchanged file must not drift it).
        let html = render_artifact(
            "demo",
            "<p>x</p>",
            "memory-user",
            &[],
            Some(0.5),
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let once = set_salience(&html, false, 0.63);
        let f1 = crate::parser::extract(&once);
        assert_eq!(f1.kb_salience, Some(0.63));
        // Simulate a second reindex of the SAME bytes (no further edit) —
        // extraction is pure, so it must be bit-identical.
        let f2 = crate::parser::extract(&once);
        assert_eq!(f1.kb_salience, f2.kb_salience);
        // And a follow-up edit replaces cleanly, not additively.
        let twice = set_salience(&once, false, 0.10);
        let f3 = crate::parser::extract(&twice);
        assert_eq!(f3.kb_salience, Some(0.10));
        assert_eq!(
            twice.matches("kb-salience").count(),
            1,
            "no duplicate meta tags"
        );
    }

    // ==== MI-W2.3 — soft forget ============================================

    #[test]
    fn mark_forgotten_html_splices_status_and_timestamp() {
        let html = render_artifact(
            "demo",
            "<p>hello</p>",
            "memory-user",
            &[],
            Some(0.7),
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let out = mark_forgotten(&html, false, 1_700_000_000);
        assert!(out.contains(r#"<meta name="kb-status" content="forgotten">"#));
        assert!(out.contains(r#"<meta name="kb-forgotten-at" content="1700000000">"#));
        // Round-trips through the parser like any other meta.
        let f = crate::parser::extract(&out);
        assert_eq!(f.kb_status.as_deref(), Some("forgotten"));
        // Untouched: title/body/other metas survive verbatim.
        assert!(out.contains("<h1>demo</h1>"));
        assert!(out.contains("kb-salience"));
    }

    #[test]
    fn mark_forgotten_markdown_writes_frontmatter() {
        let md = "---\nkb-category: memory-user\n---\n\n# demo\n\nhello\n";
        let out = mark_forgotten(md, true, 1_700_000_000);
        assert!(out.contains("kb-status: forgotten"));
        assert!(out.contains("kb-forgotten-at: 1700000000"));
        assert!(
            out.contains("kb-category: memory-user"),
            "existing frontmatter survives"
        );
        assert!(out.contains("hello"), "body untouched");
    }

    /// Re-forgetting rewrites the timestamp — not a byte-identical no-op —
    /// but the status stays exactly "forgotten" either way.
    #[test]
    fn mark_forgotten_is_idempotent_in_effect_not_in_bytes() {
        let html = render_artifact(
            "demo",
            "<p>x</p>",
            "memory-user",
            &[],
            None,
            None,
            None,
            None,
            false,
            &[],
            None,
            None,
            None,
            None,
            None,
        );
        let once = mark_forgotten(&html, false, 1_000);
        let twice = mark_forgotten(&once, false, 2_000);
        assert_ne!(once, twice, "the timestamp splice changes the bytes");
        assert!(twice.contains(r#"content="forgotten""#));
        assert!(twice.contains(r#"kb-forgotten-at" content="2000""#));
        assert!(!twice.contains(r#"kb-forgotten-at" content="1000""#));
    }

    // ==== MI-W3.1 — cross-corpus duplicate report ==========================

    fn dupe(kb: &str, id: &str, vec: Vec<f32>) -> DupeCandidate {
        DupeCandidate {
            kb: kb.into(),
            id: id.into(),
            title: format!("title-{id}"),
            forgotten: false,
            supersedes: None,
            embedding: Some(vec),
        }
    }

    #[test]
    fn cosine_similarity_basic_cases() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!((cosine_similarity(&[1.0, 0.0], &[0.0, 1.0])).abs() < 1e-6);
        assert_eq!(
            cosine_similarity(&[1.0, 0.0], &[1.0]),
            0.0,
            "length mismatch"
        );
        assert_eq!(cosine_similarity(&[], &[]), 0.0, "empty vectors");
        assert_eq!(
            cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]),
            0.0,
            "zero vector"
        );
    }

    #[test]
    fn find_duplicate_pairs_reports_high_similarity_pair() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let b = dupe("g", "b", vec![0.99_f32, 0.01_f32]);
        let pairs = find_duplicate_pairs(vec![a, b], 0.9, 10);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].id_a, "a");
        assert_eq!(pairs[0].id_b, "b");
        assert!(!pairs[0].cross_corpus);
    }

    #[test]
    fn find_duplicate_pairs_flags_cross_corpus_distinctly() {
        let a = dupe("memory", "a", vec![1.0, 0.0]);
        let b = dupe("memory-kb", "b", vec![1.0, 0.0]);
        let pairs = find_duplicate_pairs(vec![a, b], 0.9, 10);
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].cross_corpus);
    }

    #[test]
    fn find_duplicate_pairs_below_threshold_is_dropped() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let b = dupe("g", "b", vec![0.0, 1.0]);
        let pairs = find_duplicate_pairs(vec![a, b], 0.9, 10);
        assert!(pairs.is_empty());
    }

    #[test]
    fn find_duplicate_pairs_drops_forgotten_candidates() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let mut b = dupe("g", "b", vec![1.0, 0.0]);
        b.forgotten = true;
        let pairs = find_duplicate_pairs(vec![a, b], 0.9, 10);
        assert!(
            pairs.is_empty(),
            "a forgotten sibling must never be reported"
        );
    }

    #[test]
    fn find_duplicate_pairs_skips_already_linked_pairs_either_direction() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let mut b = dupe("g", "b", vec![1.0, 0.0]);
        b.supersedes = Some("a".into());
        let pairs = find_duplicate_pairs(vec![a.clone(), b], 0.9, 10);
        assert!(pairs.is_empty(), "b-supersedes-a must be skipped");

        // Reverse direction — a supersedes b.
        let mut a2 = a;
        a2.supersedes = Some("b".into());
        let b2 = dupe("g", "b", vec![1.0, 0.0]);
        let pairs2 = find_duplicate_pairs(vec![a2, b2], 0.9, 10);
        assert!(pairs2.is_empty(), "a-supersedes-b must also be skipped");
    }

    // MI-W3.R — the same-corpus test above pins the exclusion, but the
    // feature's whole motivation is that ~72% of real supersede links are
    // CROSS-corpus after the W0.4 re-home (`supersedes` is compared by id
    // alone in `find_duplicate_pairs`, never scoped to `kb` — see the
    // `a.supersedes.as_deref() == Some(b.id.as_str())` check above). Same
    // shape as `find_duplicate_pairs_skips_already_linked_pairs_either_
    // direction`, but `a.kb` and `b.kb` differ.
    #[test]
    fn find_duplicate_pairs_skips_already_linked_pairs_across_corpora() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let mut b = dupe("p", "b", vec![1.0, 0.0]);
        b.supersedes = Some("a".into());
        let pairs = find_duplicate_pairs(vec![a.clone(), b], 0.9, 10);
        assert!(
            pairs.is_empty(),
            "cross-corpus b-supersedes-a must be skipped"
        );

        // Reverse direction — a (in "g") supersedes b (in "p").
        let mut a2 = a;
        a2.supersedes = Some("b".into());
        let b2 = dupe("p", "b", vec![1.0, 0.0]);
        let pairs2 = find_duplicate_pairs(vec![a2, b2], 0.9, 10);
        assert!(
            pairs2.is_empty(),
            "cross-corpus a-supersedes-b must also be skipped"
        );
    }

    #[test]
    fn find_duplicate_pairs_skips_missing_embeddings() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let mut b = dupe("g", "b", vec![1.0, 0.0]);
        b.embedding = None;
        let pairs = find_duplicate_pairs(vec![a, b], 0.9, 10);
        assert!(pairs.is_empty());
    }

    #[test]
    fn find_duplicate_pairs_skips_mismatched_dims_not_scored_as_zero() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let b = dupe("p", "b", vec![1.0, 0.0, 0.0]);
        let pairs = find_duplicate_pairs(vec![a, b], 0.0, 10);
        assert!(
            pairs.is_empty(),
            "mismatched embedding dims must be skipped, not scored"
        );
    }

    #[test]
    fn find_duplicate_pairs_sorts_by_cosine_desc_then_id_asc() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let b = dupe("g", "b", vec![0.99, 0.14]); // slightly off — lower cosine
        let c = dupe("g", "c", vec![1.0, 0.0]); // identical to a — cosine 1.0
        let pairs = find_duplicate_pairs(vec![a, b, c], 0.5, 10);
        // All three pairwise comparisons clear the 0.5 floor: a-c (1.0),
        // a-b and b-c (both ~0.99015, since c is identical to a).
        assert_eq!(pairs.len(), 3);
        assert_eq!(
            (pairs[0].id_a.as_str(), pairs[0].id_b.as_str()),
            ("a", "c"),
            "the identical pair sorts first (cosine 1.0)"
        );
        assert!((pairs[0].cosine - 1.0).abs() < 1e-6);
        // a-b and b-c tie on cosine — id-ascending tie-break puts a-b first.
        assert!((pairs[1].cosine - pairs[2].cosine).abs() < 1e-6);
        assert_eq!((pairs[1].id_a.as_str(), pairs[1].id_b.as_str()), ("a", "b"));
        assert_eq!((pairs[2].id_a.as_str(), pairs[2].id_b.as_str()), ("b", "c"));
        assert!(pairs[0].cosine >= pairs[1].cosine);
    }

    #[test]
    fn find_duplicate_pairs_truncates_to_limit() {
        let mut candidates = Vec::new();
        for i in 0..6 {
            candidates.push(dupe("g", &format!("m{i}"), vec![1.0, 0.0]));
        }
        let pairs = find_duplicate_pairs(candidates, 0.9, 3);
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn find_duplicate_pairs_never_pairs_a_candidate_with_itself() {
        let a = dupe("g", "a", vec![1.0, 0.0]);
        let pairs = find_duplicate_pairs(vec![a], 0.0, 10);
        assert!(
            pairs.is_empty(),
            "a single candidate has no partner to pair with"
        );
    }

    #[test]
    fn find_duplicate_pairs_is_deterministic() {
        let mk = || {
            vec![
                dupe("g", "a", vec![1.0, 0.0]),
                dupe("g", "b", vec![0.95, 0.05]),
                dupe("p", "c", vec![1.0, 0.0]),
            ]
        };
        assert_eq!(
            find_duplicate_pairs(mk(), 0.5, 10),
            find_duplicate_pairs(mk(), 0.5, 10)
        );
    }

    // === MI-W4.0 — census/dupes category predicate must track recall =====

    #[test]
    fn is_recallable_memory_category_excludes_only_memory_session() {
        assert!(!is_recallable_memory_category(Some(
            crate::sessions::MEMORY_SESSION_CATEGORY
        )));
    }

    #[test]
    fn is_recallable_memory_category_admits_non_memory_prefixed_categories() {
        // The bug this predicate fixes: a memory-scoped corpus row tagged
        // with a category that does NOT start with "memory-" (e.g. the live
        // corpus's "project"-categoried rows) is exactly as recallable as
        // one tagged "memory-user" — recall only excludes session
        // transcripts, never gates on a "memory-" prefix.
        assert!(is_recallable_memory_category(Some("project")));
        assert!(is_recallable_memory_category(Some("note")));
        assert!(is_recallable_memory_category(Some("memory-user")));
    }

    #[test]
    fn is_recallable_memory_category_admits_no_category_at_all() {
        // recall's own filter only skips a Some(MEMORY_SESSION_CATEGORY)
        // exact match; a doc with NO kb-category meta is not skipped either.
        assert!(is_recallable_memory_category(None));
    }

    // === MI-W5.1 — milestone-gate fixture bench =============================
    //
    // Five hand-curated scenarios modelled on the LongMemEval memory
    // abilities + the STALE conflict taxonomy (ratified plan, W5.1), pinned
    // as fast, deterministic, LLM-free Rust tests so kb can regression-test
    // memory correctness in CI — no daemon, no live corpus, no judge. Each
    // scenario that the v2 flags touch is run under ALL FOUR
    // (relevance, stability) combinations (MI-W5.R — the flags are
    // independent, so a regression that only shows up in one combination,
    // e.g. relevance-on/stability-off, the shipped default, must be caught
    // too — not just the two single-flag-together states the pre-split
    // fixture exercised).
    //
    // Two things this file's pure functions structurally cannot exercise —
    // real engine-score abstention on an off-corpus query, and the measured
    // v1-vs-v2 A/B on the real memory corpora — are covered by the W5.1
    // gate's live-daemon probe instead (not a fixture: BM25/vector scoring
    // has no LLM-free unit-testable oracle, and "did this actually change
    // real rankings" requires the real corpus).

    /// LongMemEval "information update": after A supersedes B supersedes C,
    /// recall for the topic returns ONLY A. `Storage::list_supersede_
    /// targets` (storage/lance.rs) collects EVERY non-empty `kb_supersedes`
    /// value across the whole corpus in one flat scalar scan — not just the
    /// chain head's pointer — so a multi-hop chain tombstones every
    /// non-head link without needing a recursive walk. This fixture builds
    /// that same flattened set by hand (the exact shape the recall route
    /// passes to `rerank_with_policy_scored`) rather than standing up
    /// storage, so it stays a pure-function test.
    #[test]
    fn fixture_supersede_chain_resolves_to_only_the_head() {
        let hits = vec![
            hit("g", "a-current", 0),
            hit("g", "b-old", 1),
            hit("g", "c-oldest", 2),
        ];
        // a-current.kb_supersedes = "b-old"; b-old.kb_supersedes =
        // "c-oldest" — the flattened tombstone set is every TARGET, i.e.
        // both non-head ids, exactly what `list_supersede_targets` returns.
        let tombstones: HashSet<String> = ["b-old".to_string(), "c-oldest".to_string()]
            .into_iter()
            .collect();

        for (relevance, stability) in ALL_FOUR_FLAG_COMBINATIONS {
            let out = rerank_with_policy_scored(
                hits.clone(),
                &tombstones,
                NOW,
                10,
                DecayPolicy::Balanced,
                relevance,
                stability,
            );
            let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["a-current"],
                "(relevance={relevance}, stability={stability}): only the chain head must \
                 survive a 3-hop supersede chain"
            );
        }
    }

    /// LongMemEval "abstention", pure-function half: `rerank_with_policy_
    /// scored` only ever reorders/drops hits the search engine already
    /// returned — it structurally cannot invent one — so an empty hit list
    /// (the real "never-mentioned-topic" case at the search layer) always
    /// recalls nothing, and every id it DOES return must already have been
    /// in its input. The engine-level half — a genuinely off-corpus query
    /// returning zero search hits in the first place — is not a
    /// rerank-layer property; it's verified against the live corpus in the
    /// W5.1 A/B instead.
    #[test]
    fn fixture_abstention_empty_input_yields_empty_output_both_flag_states() {
        for (relevance, stability) in ALL_FOUR_FLAG_COMBINATIONS {
            let out = rerank_with_policy_scored(
                Vec::new(),
                &HashSet::new(),
                NOW,
                10,
                DecayPolicy::Balanced,
                relevance,
                stability,
            );
            assert!(
                out.is_empty(),
                "(relevance={relevance}, stability={stability}): no input, no hits"
            );
        }
    }

    #[test]
    fn fixture_no_hallucination_output_ids_are_a_subset_of_input_ids() {
        let hits = vec![hit("g", "x", 0), hit("g", "y", 1), hit("g", "z", 2)];
        let input_ids: HashSet<&str> = hits.iter().map(|h| h.id.as_str()).collect();
        for (relevance, stability) in ALL_FOUR_FLAG_COMBINATIONS {
            let out = rerank_with_policy_scored(
                hits.clone(),
                &HashSet::new(),
                NOW,
                10,
                DecayPolicy::Balanced,
                relevance,
                stability,
            );
            for s in &out {
                assert!(
                    input_ids.contains(s.id.as_str()),
                    "(relevance={relevance}, stability={stability}): rerank produced an id \
                     ({}) never in its input",
                    s.id
                );
            }
        }
    }

    /// STALE taxonomy — soft-forget: a soft-forgotten memory is dropped
    /// from every recall (all four v2 flag combinations — forgetting is
    /// orthogonal to both scoring_v2 factors) but must remain present,
    /// findable, and auditable in census. Census's own inclusion gate
    /// (`is_recallable_memory_category`) never inspects `kb-status` at all,
    /// so a forgotten row's category keeps it in the census scan exactly as
    /// it was before being forgotten — `forgotten` is surfaced as a FLAG on
    /// the row (`CensusRow::forgotten`, kb-server routes/memory.rs), never
    /// an exclusion. The full round trip (forget → recall drops it → census
    /// still lists it with `forgotten: true` → `--purge` removes it from
    /// both) is pinned end-to-end against a live daemon+storage by
    /// `memory_forget_soft_then_purge_round_trip`
    /// (kb-server/tests/end_to_end.rs) — these two fixtures are its fast,
    /// no-daemon companions pinning the same contract's two halves in
    /// isolation.
    #[test]
    fn fixture_soft_forgotten_never_surfaces_in_recall() {
        let mut gone = hit("g", "forgotten-one", 0);
        gone.status = Some("forgotten".into());
        let kept = hit("g", "kept-one", 1);
        for (relevance, stability) in ALL_FOUR_FLAG_COMBINATIONS {
            let out = rerank_with_policy_scored(
                vec![gone.clone(), kept.clone()],
                &HashSet::new(),
                NOW,
                10,
                DecayPolicy::Balanced,
                relevance,
                stability,
            );
            let ids: Vec<&str> = out.iter().map(|s| s.id.as_str()).collect();
            assert_eq!(
                ids,
                vec!["kept-one"],
                "(relevance={relevance}, stability={stability}): a forgotten memory must \
                 never surface in recall"
            );
        }
    }

    #[test]
    fn fixture_soft_forgotten_stays_in_census_eligibility() {
        // census's inclusion gate is category-only; a forgotten memory's
        // category (whatever it is) is unaffected by its forgotten status,
        // so it stays eligible for the census scan that recall (above) just
        // excluded it from.
        assert!(is_recallable_memory_category(Some("memory-user")));
        assert!(is_recallable_memory_category(None));
    }

    /// The core claim of MI-W2.1: two memories at equal salience/age where
    /// one is a MUCH better textual match. v1's only relevance signal is
    /// rank position (`rel = 1/(60+rank)`), which barely moves across an
    /// entire 20-result window — position 0 vs. position 19 differs by only
    /// ~24% (`(rel(0)-rel(19))/rel(0)`, computed below and pinned exactly so
    /// this fixture can never silently drift out of sync with the `RANK_K`
    /// constant it's exercising). v2 folds in the search engine's OWN
    /// score, min-max normalized across the survivor set — with scores this
    /// far apart (0.95 vs. 0.05) the normalization sends the poor match's
    /// `relevance_factor` to EXACTLY 0.0 (it drops out of ranking
    /// altogether), while v1 kept it competitively close despite being a
    /// much worse match.
    #[test]
    fn fixture_relevance_discrimination_v1_barely_separates_v2_sharply_ranks() {
        let rel0 = 1.0_f32 / RANK_K;
        let rel19 = 1.0_f32 / (RANK_K + 19.0);
        let v1_spread = (rel0 - rel19) / rel0;
        assert!(
            (v1_spread - 0.2405).abs() < 0.001,
            "pin the ~24% claim this fixture exercises: got {v1_spread}"
        );

        let mut good_match = hit("g", "good-match", 0);
        good_match.salience = Some(0.5);
        good_match.score = Some(0.95); // real engine relevance — a strong match
        let mut poor_match = hit("g", "poor-match", 19);
        poor_match.salience = Some(0.5);
        poor_match.score = Some(0.05); // real engine relevance — a weak match

        // v1: both v2 flags off — score is rel(rank) × salience only (no
        // age decay here; mtime is None on both, so decay = 1.0 for both).
        let v1 = rerank_with_policy_scored(
            vec![good_match.clone(), poor_match.clone()],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            false,
        );
        let v1_good = v1.iter().find(|s| s.id == "good-match").unwrap().score;
        let v1_poor = v1.iter().find(|s| s.id == "poor-match").unwrap().score;
        assert!(v1_good > v1_poor, "v1 still ranks the true match first…");
        let v1_ratio = (v1_good - v1_poor) / v1_good;
        assert!(
            (v1_ratio - v1_spread).abs() < 1e-4,
            "…but only barely: v1's separation ({v1_ratio}) is exactly the \
             rank-only spread, not the true match-quality gap"
        );

        // v2: scoring_v2_relevance on (stability off — irrelevant here,
        // recall_count defaults to 0) — the real score gets min-max
        // normalized to [0,1] over the two survivors: good→1.0, poor→0.0
        // exactly.
        let v2 = rerank_with_policy_scored(
            vec![good_match, poor_match],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            true,
            false,
        );
        let good_row = v2.iter().find(|s| s.id == "good-match").unwrap();
        let poor_row = v2.iter().find(|s| s.id == "poor-match").unwrap();
        assert_eq!(good_row.relevance_factor, Some(1.0));
        assert_eq!(poor_row.relevance_factor, Some(0.0));
        assert_eq!(
            poor_row.score, 0.0,
            "v2 sends the poor match's score to exactly zero — a clean, \
             unmistakable separation, not a ~24% edge"
        );
        assert!(good_row.score > 0.0);
    }

    /// Stability (MI-W2.2), bounded: `scoring_v2_stability` on,
    /// `scoring_v2_relevance` off (isolating the stability factor — neither
    /// hit carries a `score` anyway). Two memories differ ONLY in recall
    /// history (equal rank/salience/decay bucket). At a moderate current
    /// age the heavily-recalled one outranks a never-recalled but fresher
    /// memory (recall history is a real signal); push the SAME
    /// heavily-recalled memory's current age far enough past the crossover
    /// and it loses anyway — recall history can slow decay, it can never
    /// halt it. `stability_never_makes_a_memory_immortal` above pins the
    /// asymptotic version of this claim; this fixture pins the CONCRETE
    /// crossover point for the exact constants this milestone ships with
    /// (~12 days, at `STABILITY_GAIN_BASE`/`STABILITY_CEILING`/`DECAY_FAST`
    /// current values), so a future constant change that quietly erases the
    /// crossover — making recall history immortal in practice — is caught
    /// here, not discovered live.
    #[test]
    fn fixture_stability_crossover_is_bounded() {
        let recalled_yesterday = |age_days_now: i64| {
            let mut h = hit("g", "ancient-heavy", 0);
            h.salience = Some(0.5);
            h.decay = Some("fast".into());
            h.mtime_unix = Some(NOW - age_days_now * DAY);
            h.recall_count = 1_000; // heavily recalled — saturates the ceiling
            h.last_recalled_at = Some(NOW - DAY); // most recently recalled "yesterday"
            h
        };
        let mut fresh = hit("g", "fresh-never-recalled", 1);
        fresh.salience = Some(0.5);
        fresh.decay = Some("fast".into());
        fresh.mtime_unix = Some(NOW - DAY); // one day old, never recalled

        // Below the crossover (~12 days at these constants): the
        // ceiling-capped stability (3.0×) still keeps the ancient memory's
        // decay at its 1.0 cap, ahead of the fresh memory's already-decaying
        // ~0.905.
        let below = rerank_with_policy_scored(
            vec![recalled_yesterday(5), fresh.clone()],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            true,
        );
        assert_eq!(
            below.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["ancient-heavy", "fresh-never-recalled"],
            "at 5 days old, heavy recall history still outranks a fresher never-recalled memory"
        );

        // Past the crossover: even at the stability ceiling, exponential
        // decay wins eventually — the fresh memory (still only 1 day old)
        // overtakes the ancient one. This is the "never immortal" property
        // made concrete: a finite multiplier only buys a finite reprieve.
        let above = rerank_with_policy_scored(
            vec![recalled_yesterday(30), fresh],
            &HashSet::new(),
            NOW,
            10,
            DecayPolicy::Balanced,
            false,
            true,
        );
        assert_eq!(
            above.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["fresh-never-recalled", "ancient-heavy"],
            "at 30 days old, even the maximum stability multiplier can't save the ancient memory"
        );
    }
}
