//! Ratified sessions constants (sessions-rethink W0).
//!
//! ONE home for the values the synthesis memo froze, so the engine, the
//! renderer, the wire, the CLI and the SPA can never disagree about an anchor
//! id, a harness name, a clamp or a cap. Every item below cites the memo
//! ratification that fixed it (`~/.claude/plans/sessions-rethink-workpapers/
//! synthesis-memo.md`); changing a value here is a spec change, not a tweak.
//!
//! Nothing in this module is wired into the pipeline yet — W0 lands the
//! vocabulary; W1/W2 consume it (one engine, one migration, one reindex).

/// R3 — the id of the rendered outcome footer card (`<section id="…">`) and
/// the fragment every "jump to the end" affordance targets
/// (`…#ses-outcome`). Namespaced on purpose: the renderer's turn anchors are
/// `t-<uuid12>` / `turn-N`, so this can never collide with one.
///
/// The reader design's earlier `session-end` spelling is SUPERSEDED by this
/// value — the memo renamed it when it became a stable deep-link target.
pub const SES_OUTCOME_ANCHOR: &str = "ses-outcome";

/// R5 — the CLOSED set of harness identities a capture may declare, in the
/// canonical order the facet/glyph surfaces render them.
///
/// These are *dialects*, not drivers: `grok` covers any Grok-Build capture
/// however it was launched (the adapter may additionally record
/// `driver:"grokclaude"` in its `adapter-meta` line). A value outside this set
/// means the extraction ladder failed and the row falls back to
/// [`HARNESS_DEFAULT`].
pub const HARNESSES: [&str; 6] = ["claude", "codex", "opencode", "grok", "kimi", "omp"];

/// R5 — the harness a capture is attributed to when no adapter-meta line and
/// no `<meta name="kb-harness">` is present: a transcript that parsed as a
/// session at all, with no adapter fingerprint, came from Claude Code.
///
/// The V0029 column is `NOT NULL DEFAULT 'claude'` for the same reason —
/// un-backfilled history is honestly Claude, not NULL.
pub const HARNESS_DEFAULT: &str = "claude";

/// R6/D4 — per-delta clamp (seconds) for the honest active-time sum: a gap
/// between two consecutive transcript events longer than this counts as
/// [`ACTIVE_DELTA_CLAMP_SECS`], not as its wall-clock length (the operator
/// walked away; a 6-day multi-Stop capture must not report "169h 59m" of
/// work). Operator decision D4 resolved 5 minutes over 30.
///
/// ONE const, ONE consumer fn ([`super::active_secs`]), so the list row and
/// the reader header can never disagree.
pub const ACTIVE_DELTA_CLAMP_SECS: i64 = 300;

/// R3 — cap (chars) on the `outcome` preview field derived server-side from
/// `last_assistant_text` for list rows, `SessionOut`, `kb why` and
/// `kb recollect`. The wire preview is a display convenience; the full text
/// rides `SessionDetail`.
pub const OUTCOME_WIRE_MAX_CHARS: usize = 240;

/// R3/R5 — cap (chars) on the persisted `last_assistant_text` column (V0029).
/// Head-kept: the breadth map measured a median closing message of ~2,188
/// chars that front-loads its summary ("Done. The entire effort is …"), so a
/// head window carries the closure and the tail is recoverable from the
/// artifact itself.
pub const LAST_ASSISTANT_TEXT_MAX_CHARS: usize = 700;

/// R15/LF-1 Tier 1 — a live transcript file whose mtime is within this many
/// seconds of now is **LIVE** (a writer is active right now). Transcript-
/// derived, opt-in (`[sessions] live_transcripts_dir`), host/dev only.
pub const LIVE_WINDOW_SECS: i64 = 120;

/// R15/LF-1 Tier 0 — a session whose newest capture-derived timestamp
/// (`max(ended_at, capture mtime)`) is within this many seconds of now is
/// **ACTIVE**. Universal (works in prod and on the phone) but honest only to
/// capture cadence — the chip copy says "as of capture · Xm ago", never a
/// fake spinner.
pub const ACTIVE_WINDOW_SECS: i64 = 600;

/// LSC-1 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §3 "The
/// model: two axes, not three buckets") — how long an `Holder::Agent`
/// session may sit silent before [`super::live::derive_state`] escalates it
/// from `working` to `stalled`. Silence past this point never reclassifies
/// the HOLDER axis (a 45-minute `cargo build` appends nothing to the
/// transcript, but the agent still owns the turn) — it only decorates the
/// `working` lane with an honest doubt marker. The design's own callout:
/// "well above any plausible single tool call".
pub const STALL_AFTER_SECS: i64 = 45 * 60;

/// LSC-1 (design §3) — how long a `Holder::Human` session may sit silent
/// before [`super::live::derive_state`] escalates it from `waiting` to
/// `cold`. Chosen as "a working day's patience"; still resumable and sorted
/// below the notification-worthy `waiting` lane, never dropped.
pub const COLD_AFTER_SECS: i64 = 8 * 3600;

/// LSC-2 amendment — how long an `Holder::Agent` session may sit silent
/// before [`super::live::derive_state`] stops asserting it is in progress at
/// all and reports `presumed_ended` instead.
///
/// [`STALL_AFTER_SECS`] deliberately does NOT move the holder axis: a
/// silent agent-held session is still agent-held, because a long tool call
/// is silent by construction. But that reasoning has a horizon. The first
/// five-harness run surfaced five grok sessions whose last record was
/// `turn_started` **six days ago** — processes long dead that never wrote a
/// close — sitting at the top of the IN PROGRESS lane. No tool call runs
/// for six days, so "the agent still owns the turn" had stopped being an
/// honest reading of the evidence.
///
/// This is the design's own `PresumedEnded`: distinct from `Finished`
/// (which requires an explicit end signal) precisely because it is an
/// INFERENCE from a missing close, and the two must never be rendered as
/// the same claim. Set at the same horizon as [`COLD_AFTER_SECS`] — past a
/// working day, an unclosed turn is a corpse, not a build.
pub const ABANDON_AFTER_SECS: i64 = 8 * 3600;

/// LSC-1 (design §12 "Evidence appendix" — the validated Python spike this
/// module's `classify_claude_transcript` ports to Rust) — the byte budget
/// [`super::live::classify_claude_transcript`] reads from the END of a
/// transcript file, never more. The design's measured rationale: 191 MB of
/// transcripts on this box, 999 subagent/workflow files nested beneath 55
/// real sessions — a live-status classifier that ever full-parses is both
/// wrong (subagent transcripts aren't sessions) and too slow to poll.
pub const LIVE_TAIL_BYTES: u64 = 256 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// R5's ONE golden: the harness set and its default are a frozen contract
    /// shared by the V0029 column default, the capture adapters, the facet UI
    /// and the CLI filter. Adding a harness is a migration-visible decision —
    /// this test is the tripwire.
    #[test]
    fn harness_set_and_default_are_pinned() {
        assert_eq!(
            HARNESSES,
            ["claude", "codex", "opencode", "grok", "kimi", "omp"]
        );
        assert_eq!(HARNESS_DEFAULT, "claude");
        assert!(
            HARNESSES.contains(&HARNESS_DEFAULT),
            "the default harness must be a member of the closed set"
        );
        // The set is a SET: no duplicates, and the declared order is the
        // render order (never sorted at use sites).
        let mut sorted = HARNESSES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), HARNESSES.len(), "harness names are distinct");
    }

    /// The anchor id is a deep-link target (`?turn=`/`#ses-outcome` grammar) —
    /// pin the exact spelling, and that it is fragment-safe.
    #[test]
    fn outcome_anchor_is_pinned_and_fragment_safe() {
        assert_eq!(SES_OUTCOME_ANCHOR, "ses-outcome");
        assert!(SES_OUTCOME_ANCHOR
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    /// LSC-1's three ratified thresholds: pin the exact values the design
    /// round chose. The pinned values themselves establish the relative
    /// ordering that matters (a stall must trigger long before a plausible
    /// single tool call finishes; cold must be well beyond a lunch break,
    /// not just a coffee break) — a separate `COLD_AFTER_SECS >
    /// STALL_AFTER_SECS` assertion would just be clippy's
    /// `assertions_on_constants` lint restating a fact these two lines
    /// already fix.
    #[test]
    fn live_thresholds_are_pinned() {
        assert_eq!(STALL_AFTER_SECS, 45 * 60);
        assert_eq!(COLD_AFTER_SECS, 8 * 3600);
        assert_eq!(ABANDON_AFTER_SECS, 8 * 3600);
        assert_eq!(LIVE_TAIL_BYTES, 256 * 1024);
    }
}
