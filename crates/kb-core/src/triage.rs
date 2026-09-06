//! MI-W4.4 — the memory hygiene queue: a bounded, Anki-style triage list of
//! "the memories most worth 90 seconds right now", each with a one-line
//! JUSTIFICATION of why it's in the queue. Mirrors [`crate::resurface`]'s
//! shape deliberately: pure, deterministic scoring core taking
//! caller-gathered candidates + `now_unix`, no state of its own — the queue
//! is DERIVED per request, never stored, and this module never mutates
//! anything. The route (`kb-server/src/routes/memory.rs::triage`) gathers
//! candidates from the SAME data the census/dupes/lineage routes already
//! read; the CLI (`kb memory triage`) renders the identical list + reasons.
//!
//! Every candidate collapses to AT MOST one reason — the single MOST URGENT
//! one that applies — because the queue is meant to read as one clear nudge
//! per item, not a stacked audit; `build_queue`'s doc comment covers the
//! selection rule in full.

/// Default bounded queue size — "an Anki-style ~10 items", not a full audit.
pub const DEFAULT_QUEUE_SIZE: usize = 10;
/// Salience at/above which "never/rarely recalled" is worth flagging as
/// likely dead weight — a LOW-salience memory sitting unused is expected
/// (that's what the decay floor is for); a HIGH-salience one sitting unused
/// is the surprising, worth-a-look case.
pub const HIGH_SALIENCE_THRESHOLD: f32 = 0.7;
/// A memory not recalled in this many days counts as "dormant" for the
/// high-salience-dormant reason (never-recalled counts regardless of age).
pub const DORMANT_DAYS: f32 = 60.0;
/// Fixed urgency for the "superseded but not forgotten" housekeeping
/// reason — deliberately mid-scale (comparable to a middling salience),
/// not an alarm.
pub const SUPERSEDED_URGENCY: f32 = 0.5;
/// Urgency for [`TriageReason::BelowFloorNow`] — pinned to the top of the
/// scale among the queue's own HEURISTIC reasons (alongside a near-certain
/// duplicate) because, unlike below/dormant/duplicate/superseded, it isn't
/// a nudge: the memory is LITERALLY invisible to recall at this moment.
/// (CT-C1: [`FLAGGED_URGENCY`] now sits strictly above this — a flag isn't
/// a queue-computed heuristic at all, it's a human/agent's direct claim.)
pub const BELOW_FLOOR_URGENCY: f32 = 1.0;
/// Urgency for [`TriageReason::Flagged`] — CT-C1: deliberately ABOVE every
/// heuristic reason's `[0,1]` scale (including [`BELOW_FLOOR_URGENCY`]), so
/// a flagged memory ranks strictly first in the queue even when it's ALSO
/// below the floor / a duplicate / superseded. Every other reason here is
/// the queue's OWN nudge; a flag is an operator-action item someone already
/// raised by hand — it doesn't compete on the same scale, it wins outright.
pub const FLAGGED_URGENCY: f32 = 2.0;
/// CT-C1 — cap for the flag-reason excerpt rendered in
/// [`TriageReason::Flagged`]'s justification: a one-line nudge, not the
/// whole comment body.
pub const FLAG_REASON_EXCERPT_CHARS: usize = 140;

/// Why ONE candidate is in the queue, carrying the exact numbers the
/// justification text is built from — "show the ranking terms" (the
/// `kb resurface --explain` precedent), never a re-derivation on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum TriageReason {
    /// The candidate's RAW (author-set) salience is at/below the active
    /// policy's floor RIGHT NOW (`memory::floor_state` returned `Below`) —
    /// it is excluded from `recall` today, not at some future date. Decay
    /// never lowers the value the floor tests (see `memory::decay_half_
    /// life_days`'s module doc for the ground truth this reason is built
    /// on), so there is no "days until" quantity to report here — either a
    /// memory's salience already clears the floor (and, absent an explicit
    /// salience edit, always will) or it doesn't.
    BelowFloorNow { salience: f32, floor: f32 },
    /// Salience `>= HIGH_SALIENCE_THRESHOLD` but never recalled, or not
    /// recalled in >= [`DORMANT_DAYS`]. `dormant_days` is `None` for
    /// "never recalled at all" (there's no elapsed-since-last to report).
    HighSalienceDormant {
        salience: f32,
        dormant_days: Option<f32>,
    },
    /// Flagged by the MI-W3.1 duplicate scan as a likely-redundant sibling.
    Duplicate { other_id: String, cosine: f32 },
    /// Something else's `kb-supersedes` names this memory, but this memory
    /// hasn't been soft-forgotten (MI-W2.3) — the natural next step a human
    /// forgot to take.
    SupersededNotForgotten { superseded_by: String },
    /// CT-C1 — an agent flagged this memory WRONG mid-session (`kb memory
    /// flag`, an ordinary open `[kb-flag]` comment — kb-comments/1,
    /// invariant #6). `reason` is the flag's own (untruncated) text; the
    /// justification excerpts it. An operator-action item, not a heuristic
    /// — see [`FLAGGED_URGENCY`].
    Flagged { reason: String },
}

impl TriageReason {
    /// A cross-category urgency score, roughly `[0,1]` for every variant so
    /// candidates from different reasons sort against each other sanely —
    /// NOT a scientifically unified utility function, just "sooner /
    /// higher-salience / more-similar / housekeeping-baseline" in that
    /// rough order, transparently from the SAME numbers the justification
    /// text renders (no hidden term).
    pub fn urgency(&self) -> f32 {
        match self {
            TriageReason::BelowFloorNow { .. } => BELOW_FLOOR_URGENCY,
            TriageReason::HighSalienceDormant { salience, .. } => salience.clamp(0.0, 1.0),
            TriageReason::Duplicate { cosine, .. } => cosine.clamp(0.0, 1.0),
            TriageReason::SupersededNotForgotten { .. } => SUPERSEDED_URGENCY,
            TriageReason::Flagged { .. } => FLAGGED_URGENCY,
        }
    }

    /// The one-line human-readable justification.
    pub fn justification(&self) -> String {
        match self {
            TriageReason::BelowFloorNow { salience, floor } => {
                format!(
                    "salience {salience:.2} is at/below the {floor:.2} floor — excluded from recall now"
                )
            }
            TriageReason::HighSalienceDormant {
                salience,
                dormant_days,
            } => match dormant_days {
                Some(d) => format!("salience {salience:.2} but last recalled {d:.0}d ago"),
                None => format!("salience {salience:.2} but never recalled"),
            },
            TriageReason::Duplicate { other_id, cosine } => {
                format!("flagged duplicate of {other_id} (cosine {cosine:.2})")
            }
            TriageReason::SupersededNotForgotten { superseded_by } => {
                format!("superseded by {superseded_by} — not yet forgotten")
            }
            TriageReason::Flagged { reason } => {
                format!("flagged: {}", excerpt_reason(reason))
            }
        }
    }

    /// Stable machine-readable tag for the wire/CLI `--json` output.
    pub fn kind(&self) -> &'static str {
        match self {
            TriageReason::BelowFloorNow { .. } => "below_floor_now",
            TriageReason::HighSalienceDormant { .. } => "high_salience_dormant",
            TriageReason::Duplicate { .. } => "duplicate",
            TriageReason::SupersededNotForgotten { .. } => "superseded_not_forgotten",
            TriageReason::Flagged { .. } => "flagged",
        }
    }
}

/// CT-C1 — collapse internal whitespace and cap at
/// [`FLAG_REASON_EXCERPT_CHARS`] (char-count, `…`-suffixed) for the
/// justification line. Mirrors `kb_core`'s other excerpt-capping helpers
/// (e.g. `lists::section_passage`'s), kept local since this is the only
/// caller.
fn excerpt_reason(reason: &str) -> String {
    let collapsed: String = reason.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= FLAG_REASON_EXCERPT_CHARS {
        return collapsed;
    }
    let capped: String = collapsed.chars().take(FLAG_REASON_EXCERPT_CHARS).collect();
    format!("{capped}…")
}

/// Raw per-memory inputs the route gathers from the same reads
/// census/dupes/lineage already use. Every field the queue doesn't need is
/// deliberately absent (unlike `CensusRow`, this isn't a display shape).
#[derive(Debug, Clone)]
pub struct TriageCandidate {
    pub kb: String,
    pub id: String,
    pub title: String,
    /// Pinned and forgotten memories never enter the queue at all — see
    /// [`build_queue`]'s doc comment — but the caller passes them through
    /// anyway (rather than filtering upstream) so a candidate list built by
    /// simply mapping every census row never has to special-case this.
    pub pinned: bool,
    pub forgotten: bool,
    pub salience: Option<f32>,
    pub recall_count: u32,
    pub last_recalled_at: Option<i64>,
    /// Reverse `kb-supersedes` lookup (mirrors `CensusRow::superseded_by`):
    /// `Some(other_id)` when some OTHER memory names this one as superseded.
    pub superseded_by: Option<String>,
    /// The single highest-cosine duplicate pairing involving this memory
    /// (from `memory::find_duplicate_pairs`), if any.
    pub dupe_of: Option<(String, f32)>,
    /// CT-C1 — `Some(reason)` when this candidate has at least one OPEN
    /// `[kb-flag]` comment (`kb_core::memory::FLAG_COMMENT_PREFIX`); the
    /// text is the flag's own (untruncated) body — [`TriageReason::
    /// Flagged`]'s justification excerpts it for display. Deliberately
    /// independent of every other field here: those all come from a bulk
    /// lance/embedding read, this one comes from a SEPARATE, bounded
    /// `.review/` directory scan (the route's own helper), so a caller
    /// mapping straight off `list_docs` rows never has to special-case it.
    pub flagged_reason: Option<String>,
}

/// One queued item: the candidate's identity plus the single reason chosen
/// for it and that reason's urgency (duplicated onto the struct so a caller
/// sorts/paginates without re-matching `reason`).
#[derive(Debug, Clone)]
pub struct TriageItem {
    pub kb: String,
    pub id: String,
    pub title: String,
    pub reason: TriageReason,
    pub urgency: f32,
}

/// Compute every applicable reason for one candidate, given the ACTIVE
/// decay policy's floor for its corpus (`None` = `Loose`, never excludes on
/// salience grounds). `now_unix` only affects the dormant-days term (a
/// wall-clock read), mirroring `resurface::score`'s explicit-clock
/// convention. Pure — a caller unit-testing `build_queue`'s selection rule
/// (below) doesn't need this exposed, but it's `pub` so the route/CLI's
/// `--explain`-style rendering can recompute the FULL reason set for one
/// item on demand without duplicating this matching logic.
pub fn reasons_for(c: &TriageCandidate, now_unix: i64, floor: Option<f32>) -> Vec<TriageReason> {
    let mut out = Vec::new();
    if c.pinned || c.forgotten {
        return out;
    }
    if let Some(reason) = &c.flagged_reason {
        out.push(TriageReason::Flagged {
            reason: reason.clone(),
        });
    }
    // MI-W4.1(revision) — `floor_state` is evaluated against the candidate's
    // RAW salience (a constant), never a projected/decayed value — `pinned`
    // is always `false` here (checked above), so this can only resolve to
    // `NoFloor`/`Above`/`Below`.
    if let Some(salience) = c.salience {
        if let crate::memory::FloorState::Below { salience, floor } =
            crate::memory::floor_state(salience, floor, false)
        {
            out.push(TriageReason::BelowFloorNow { salience, floor });
        }
    }
    if let Some(salience) = c.salience {
        if salience >= HIGH_SALIENCE_THRESHOLD {
            let dormant_days = c
                .last_recalled_at
                .map(|t| (now_unix - t).max(0) as f32 / 86_400.0);
            let dormant = match dormant_days {
                None => c.recall_count == 0,
                Some(d) => d >= DORMANT_DAYS,
            };
            if dormant {
                out.push(TriageReason::HighSalienceDormant {
                    salience,
                    dormant_days,
                });
            }
        }
    }
    if let Some((other_id, cosine)) = &c.dupe_of {
        out.push(TriageReason::Duplicate {
            other_id: other_id.clone(),
            cosine: *cosine,
        });
    }
    if let Some(superseded_by) = &c.superseded_by {
        out.push(TriageReason::SupersededNotForgotten {
            superseded_by: superseded_by.clone(),
        });
    }
    out
}

/// Build the bounded, ranked hygiene queue.
///
/// - Pinned and forgotten candidates NEVER appear — pinning is a deliberate
///   keep-decision the queue shouldn't second-guess, and a forgotten memory
///   is already at its terminal state.
/// - A candidate with more than one applicable reason (e.g. is below the
///   floor now AND is a flagged duplicate) keeps only its MOST URGENT one
///   — the queue is one clear nudge per item, not a stacked audit; a caller
///   wanting the full reason set for one specific item can call
///   [`reasons_for`] directly.
/// - Ranked by urgency desc, `(kb, id)` asc tie-break — deterministic.
/// - Truncated to `limit` — this is the ONLY place size is bounded; the
///   function never mutates anything and holds no state of its own (it's a
///   VIEW over whatever `candidates` the caller already read this request).
pub fn build_queue(
    candidates: &[TriageCandidate],
    now_unix: i64,
    floor_by_kb: &std::collections::HashMap<String, Option<f32>>,
    limit: usize,
) -> Vec<TriageItem> {
    let mut out: Vec<TriageItem> = candidates
        .iter()
        .filter_map(|c| {
            let floor = floor_by_kb.get(&c.kb).copied().flatten();
            let reason = reasons_for(c, now_unix, floor)
                .into_iter()
                .max_by(|a, b| a.urgency().total_cmp(&b.urgency()))?;
            Some(TriageItem {
                kb: c.kb.clone(),
                id: c.id.clone(),
                title: c.title.clone(),
                urgency: reason.urgency(),
                reason,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.urgency
            .total_cmp(&a.urgency)
            .then_with(|| a.kb.cmp(&b.kb))
            .then_with(|| a.id.cmp(&b.id))
    });
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str) -> TriageCandidate {
        TriageCandidate {
            kb: "g".to_string(),
            id: id.to_string(),
            title: format!("title-{id}"),
            pinned: false,
            forgotten: false,
            salience: None,
            recall_count: 0,
            last_recalled_at: None,
            superseded_by: None,
            dupe_of: None,
            flagged_reason: None,
        }
    }

    const NOW: i64 = 1_700_000_000;
    const DAY: i64 = 86_400;

    #[test]
    fn pinned_never_enters_the_queue_even_with_every_reason_present() {
        let mut c = candidate("pinned-one");
        c.pinned = true;
        c.salience = Some(0.05);
        c.superseded_by = Some("other".into());
        c.dupe_of = Some(("dupe-id".into(), 0.99));
        c.flagged_reason = Some("wrong info".into());
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(
            q.is_empty(),
            "pinned candidate leaked into the queue: {q:?}"
        );
    }

    #[test]
    fn forgotten_never_enters_the_queue() {
        let mut c = candidate("forgotten-one");
        c.forgotten = true;
        c.superseded_by = Some("other".into());
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn a_candidate_with_no_applicable_reason_is_dropped_silently() {
        let mut c = candidate("fine");
        c.salience = Some(0.5);
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn below_floor_now_fires_when_salience_is_at_or_under_the_active_floor() {
        let mut c = candidate("under-floor");
        c.salience = Some(0.10);
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1, "queue: {q:?}");
        assert_eq!(q[0].id, "under-floor");
        assert_eq!(q[0].reason.kind(), "below_floor_now");
        assert_eq!(
            q[0].reason.justification(),
            "salience 0.10 is at/below the 0.15 floor — excluded from recall now"
        );
    }

    /// THE ground-truth property this reason exists to honour: a memory
    /// whose salience clears the floor must NEVER be flagged as excluded,
    /// no matter how old it is — decay never lowers the value the floor
    /// tests. `TriageCandidate` doesn't even carry an age field any more
    /// (removed alongside `days_until_floor_crossing`), so there is no
    /// dial a caller could turn to make this fire for a high-salience
    /// memory in the first place.
    #[test]
    fn a_high_salience_memory_is_never_flagged_below_floor_regardless_of_recall_history() {
        let mut c = candidate("old-but-important");
        c.salience = Some(0.95);
        // Recently recalled so the high-salience-dormant reason (a SEPARATE
        // check) doesn't also fire and confuse this test's assertion.
        c.recall_count = 5;
        c.last_recalled_at = Some(NOW - 2 * DAY);
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(
            q.is_empty(),
            "a high-salience memory can never be predicted to drop: {q:?}"
        );
    }

    #[test]
    fn loose_policy_never_produces_a_below_floor_reason() {
        let mut c = candidate("never-excluded-under-loose");
        c.salience = Some(0.01);
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), None); // Loose
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn high_salience_never_recalled_fires() {
        let mut c = candidate("hot-but-unused");
        c.salience = Some(0.9);
        c.recall_count = 0;
        c.last_recalled_at = None;
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].reason.kind(), "high_salience_dormant");
        assert_eq!(
            q[0].reason.justification(),
            "salience 0.90 but never recalled"
        );
    }

    #[test]
    fn high_salience_recently_recalled_does_not_fire() {
        let mut c = candidate("hot-and-used");
        c.salience = Some(0.9);
        c.recall_count = 5;
        c.last_recalled_at = Some(NOW - 2 * DAY);
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(
            q.is_empty(),
            "recently-recalled high-salience memory should not be flagged: {q:?}"
        );
    }

    #[test]
    fn low_salience_never_recalled_does_not_fire_the_dormant_reason() {
        // Below HIGH_SALIENCE_THRESHOLD — never-recalled-but-unimportant is
        // the expected, boring case the decay floor already handles.
        let mut c = candidate("cold-and-unused");
        c.salience = Some(0.3);
        c.recall_count = 0;
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn duplicate_reason_renders_the_other_id_and_cosine() {
        let mut c = candidate("dup-a");
        c.dupe_of = Some(("dup-b".to_string(), 0.97));
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1);
        assert_eq!(
            q[0].reason.justification(),
            "flagged duplicate of dup-b (cosine 0.97)"
        );
    }

    #[test]
    fn superseded_not_forgotten_reason_fires() {
        let mut c = candidate("stale-superseded");
        c.superseded_by = Some("newer-id".to_string());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].reason.kind(), "superseded_not_forgotten");
        assert_eq!(
            q[0].reason.justification(),
            "superseded by newer-id — not yet forgotten"
        );
    }

    #[test]
    fn a_candidate_with_multiple_reasons_keeps_only_the_most_urgent_one() {
        let mut c = candidate("multi");
        // A near-certain duplicate (urgency ~0.99) should beat a
        // mid-scale superseded-housekeeping reason (fixed 0.5).
        c.dupe_of = Some(("other".to_string(), 0.99));
        c.superseded_by = Some("newer".to_string());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1, "exactly one item, one reason: {q:?}");
        assert_eq!(q[0].reason.kind(), "duplicate");
    }

    // --- CT-C1 — the flagged reason ----------------------------------------

    #[test]
    fn flagged_reason_fires_and_renders_the_excerpt() {
        let mut c = candidate("flagged-one");
        c.flagged_reason = Some("this contradicts kb-docs/abc123".to_string());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].reason.kind(), "flagged");
        assert_eq!(
            q[0].reason.justification(),
            "flagged: this contradicts kb-docs/abc123"
        );
    }

    /// A flag ALWAYS wins the per-candidate max_by, even against a
    /// near-certain duplicate (urgency ~0.99) or a below-floor exclusion
    /// (urgency 1.0) on the SAME candidate — see [`FLAGGED_URGENCY`].
    #[test]
    fn flagged_reason_outranks_every_other_reason_on_the_same_candidate() {
        let mut c = candidate("multi-flagged");
        c.salience = Some(0.05); // below_floor_now, urgency 1.0
        c.dupe_of = Some(("other".to_string(), 0.99));
        c.superseded_by = Some("newer".to_string());
        c.flagged_reason = Some("wrong".to_string());
        let mut floors = std::collections::HashMap::new();
        floors.insert("g".to_string(), Some(0.15));
        let q = build_queue(&[c], NOW, &floors, 10);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].reason.kind(), "flagged");
    }

    /// Cross-candidate: a flagged memory ranks strictly ABOVE an unrelated
    /// candidate whose own reason is the heuristic max (a near-certain
    /// duplicate, urgency ~0.99) — "ranks at the top of the queue" holds
    /// across the whole queue, not just within one candidate's reason set.
    #[test]
    fn flagged_candidate_ranks_above_a_near_certain_duplicate() {
        let mut dup = candidate("near-dup");
        dup.dupe_of = Some(("other".to_string(), 0.99));
        let mut flagged = candidate("wrong-memory");
        flagged.flagged_reason = Some("stale info".to_string());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[dup, flagged], NOW, &floors, 10);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].id, "wrong-memory", "flagged must sort first: {q:?}");
        assert_eq!(q[1].id, "near-dup");
    }

    #[test]
    fn flag_reason_excerpt_is_capped_and_collapses_whitespace() {
        let long = "word ".repeat(60); // well past FLAG_REASON_EXCERPT_CHARS
        let mut c = candidate("long-flag");
        c.flagged_reason = Some(long);
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        let justification = q[0].reason.justification();
        assert!(justification.ends_with('…'), "{justification}");
        assert!(
            justification.chars().count() <= FLAG_REASON_EXCERPT_CHARS + "flagged: ".len() + 1,
            "{justification}"
        );
    }

    #[test]
    fn pinned_flagged_candidate_is_still_excluded() {
        // Same "queue never second-guesses a pin" rule every other reason
        // follows (see `pinned_never_enters_the_queue_even_with_every_
        // reason_present` above) — a flag is no exception.
        let mut c = candidate("pinned-and-flagged");
        c.pinned = true;
        c.flagged_reason = Some("wrong".to_string());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[c], NOW, &floors, 10);
        assert!(q.is_empty());
    }

    #[test]
    fn queue_is_ranked_by_urgency_descending_and_truncated_to_limit() {
        let mut low = candidate("low");
        low.superseded_by = Some("x".into()); // urgency 0.5
        let mut high = candidate("high");
        high.dupe_of = Some(("y".to_string(), 0.99)); // urgency 0.99
        let mut mid = candidate("mid");
        mid.salience = Some(0.75); // urgency 0.75, never recalled
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[low.clone(), high.clone(), mid.clone()], NOW, &floors, 2);
        assert_eq!(q.len(), 2, "truncated to limit=2");
        assert_eq!(q[0].id, "high");
        assert_eq!(q[1].id, "mid");
    }

    #[test]
    fn ties_break_on_kb_then_id_ascending_for_determinism() {
        let mut a = candidate("b");
        a.superseded_by = Some("x".into());
        let mut b = candidate("a");
        b.superseded_by = Some("x".into());
        let floors = std::collections::HashMap::new();
        let q = build_queue(&[a, b], NOW, &floors, 10);
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].id, "a");
        assert_eq!(q[1].id, "b");
    }

    #[test]
    fn build_queue_never_mutates_or_reorders_the_input_slice() {
        // `build_queue` takes `&[TriageCandidate]` — a caller's own Vec must
        // survive untouched (no `.sort()`/`.retain()` on the input).
        let candidates = vec![candidate("z"), candidate("a")];
        let ids_before: Vec<String> = candidates.iter().map(|c| c.id.clone()).collect();
        let floors = std::collections::HashMap::new();
        let _ = build_queue(&candidates, NOW, &floors, 10);
        let ids_after: Vec<String> = candidates.iter().map(|c| c.id.clone()).collect();
        assert_eq!(ids_before, ids_after);
    }
}
