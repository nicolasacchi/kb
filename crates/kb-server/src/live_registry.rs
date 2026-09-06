//! LSC-2 (`docs/research/kb-live-sessions-cockpit-2026-08.html` §6 "Shaping
//! the server" → "Storage: nothing") — the daemon-wide, in-memory live-beat
//! registry behind `POST /api/sessions/beat` / `GET
//! /api/sessions/live-status`.
//!
//! **Storage is NOTHING** (LF-7, restated in §6): no sqlite table, no
//! migration, no lance write. This is a process-local `HashMap` that a
//! daemon restart empties outright — the mitigation is the read route's
//! Tier-0 degraded layer (`routes::sessions::live_status`), rebuilt from
//! landed captures, not this module. If a daemon restart must survive, the
//! design has already ruled that out; don't reach for a file here.
//!
//! Bounded two ways so a fleet that runs for months never leaks:
//! [`REGISTRY_CAP`] entries total (LRU-by-touch eviction on overflow) and
//! [`REGISTRY_TTL_SECS`] since a session's last beat (swept on every
//! insert — no background timer, this cache is far too small to need one).
//!
//! State is derived, never stored: each row keeps only the RAW facts a beat
//! carried (holder, last-activity instant, harness, …); [`LiveRegistry::
//! snapshot`] calls [`kb_core::sessions::live::derive_state`] fresh against
//! the caller's `now_unix` every time, exactly as the design's "everything
//! the operator sees is computed, and can therefore be recomputed,
//! explained, and corrected" principle (§5) demands.

use kb_core::sessions::live::{derive_state, Confidence, Holder, LivePolicy, LiveState};
use std::collections::HashMap;
use std::sync::Mutex;

/// Cap on distinct `session_id`s held at once. The operator runs ~55
/// sessions today (design §4's measured evidence); 4096 is generous
/// headroom that still bounds a runaway fleet (or a misbehaving/duplicating
/// adapter) to a fixed, small amount of memory. Mirrors
/// `kb_core::sessions::live::LIVE_SCAN_CAP`'s order of magnitude.
pub const REGISTRY_CAP: usize = 4096;

/// A row untouched by any beat for this long is swept on the next insert.
/// 7 days: generous enough that a genuinely long-running session (or an
/// operator who steps away for a weekend) is never evicted out from under
/// itself, but small enough that a daemon left running for months doesn't
/// quietly accumulate years of dead session ids.
pub const REGISTRY_TTL_SECS: i64 = 7 * 24 * 3600;

/// One registry row — the raw facts the LAST beat for a `session_id`
/// carried. Never a derived state (see module docs); [`LiveRegistry::
/// snapshot`] is the only place `derive_state` runs against these fields.
#[derive(Debug, Clone)]
pub struct LiveRow {
    pub session_id: String,
    pub harness: String,
    pub holder: Holder,
    /// Unix seconds of the beat's own `at` timestamp — the instant
    /// `derive_state` measures silence from. NOT the same clock as
    /// `touched_unix` (which is wall-clock receipt time, the TTL/LRU
    /// clock) — a beat's `at` is adapter-supplied and could in principle
    /// lag receipt slightly; keeping them distinct mirrors
    /// `classify_claude_transcript`'s mtime-vs-now split (LSC-1 rule 4).
    pub last_activity_unix: i64,
    pub lease_secs: i64,
    pub host: Option<String>,
    pub pid: Option<i64>,
    pub cwd: Option<String>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub last_line: Option<String>,
    /// `event == "blocked"` — kept as a flag rather than a new
    /// [`kb_core::sessions::live::LiveState`] variant (the daemon's own
    /// brief: "do NOT invent a new LiveState variant"). The holder for a
    /// `blocked` beat is still `Agent` (the design's mapping table); this
    /// is display-only context a presenter may render as a distinct reason
    /// within the working/stalled lane.
    pub blocked: bool,
    pub detail_reason: Option<String>,
    /// Wall-clock receipt time (unix seconds) — the TTL/LRU-by-touch clock.
    /// Distinct from `last_activity_unix`; see that field's doc comment.
    pub touched_unix: i64,
}

/// The outcome of one [`LiveRegistry::record_beat`] call: the row as
/// stored, its freshly-derived state/confidence (against the SAME
/// `now_unix` the caller passed in), and the state the session was in
/// immediately BEFORE this beat (`None` for a session's first-ever beat).
/// The route handler diffs `previous_state` against `state` to decide
/// whether to fire the `session.state` SSE event — never per-beat, only on
/// a genuine transition (design §6 "Events").
pub struct RecordOutcome {
    pub row: LiveRow,
    pub state: LiveState,
    pub confidence: Confidence,
    pub previous_state: Option<LiveState>,
}

/// The daemon-wide live registry. `std::sync::Mutex` — every method here is
/// synchronous (no `.await` inside the critical section), so the guard
/// never crosses an await point (invariant #15).
#[derive(Default)]
pub struct LiveRegistry {
    inner: Mutex<HashMap<String, LiveRow>>,
}

impl LiveRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert/replace the row for `row.session_id`, sweeping expired rows
    /// first and evicting the least-recently-touched row if inserting a
    /// brand-new key would exceed [`REGISTRY_CAP`]. Returns the derived
    /// state before/after so the caller can detect a transition.
    pub fn record_beat(&self, row: LiveRow, now_unix: i64) -> RecordOutcome {
        let policy = LivePolicy::default();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());

        // TTL sweep — cheap at this cap (<=4096 entries), and only runs on
        // the write path, never on a read.
        g.retain(|_, r| now_unix - r.touched_unix < REGISTRY_TTL_SECS);

        let previous_state = g.get(&row.session_id).map(|old| {
            derive_state(
                old.holder,
                old.last_activity_unix,
                now_unix,
                kb_core::sessions::live::StateSource::Hook,
                &policy,
            )
            .0
        });

        // Cap enforcement: only relevant when inserting a KEY THAT DOESN'T
        // ALREADY EXIST (an update to an existing session never grows the
        // map). Evict the row with the oldest `touched_unix`.
        if !g.contains_key(&row.session_id) && g.len() >= REGISTRY_CAP {
            if let Some(oldest) = g
                .iter()
                .min_by_key(|(_, r)| r.touched_unix)
                .map(|(k, _)| k.clone())
            {
                g.remove(&oldest);
            }
        }

        let (state, confidence) = derive_state(
            row.holder,
            row.last_activity_unix,
            now_unix,
            kb_core::sessions::live::StateSource::Hook,
            &policy,
        );
        g.insert(row.session_id.clone(), row.clone());

        RecordOutcome {
            row,
            state,
            confidence,
            previous_state,
        }
    }

    /// Every row, each with a freshly-derived `(state, confidence)` against
    /// `now_unix`. Order is whatever `HashMap` iteration gives — the caller
    /// (the `live-status` route) owns filtering/sorting, matching the
    /// existing `sessions::list` fan-out's "collect then sort" shape.
    pub fn snapshot(&self, now_unix: i64) -> Vec<(LiveRow, LiveState, Confidence)> {
        let policy = LivePolicy::default();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.values()
            .map(|r| {
                let (state, confidence) = derive_state(
                    r.holder,
                    r.last_activity_unix,
                    now_unix,
                    kb_core::sessions::live::StateSource::Hook,
                    &policy,
                );
                (r.clone(), state, confidence)
            })
            .collect()
    }

    /// The set of session ids currently tracked — used by the `live-status`
    /// route to know which Tier-0 capture-derived rows to SKIP (design §6:
    /// "a registry entry always wins over a capture-derived row").
    pub fn known_session_ids(&self) -> std::collections::HashSet<String> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.keys().cloned().collect()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sid: &str, holder: Holder, last_activity_unix: i64, touched_unix: i64) -> LiveRow {
        LiveRow {
            session_id: sid.to_string(),
            harness: "claude".to_string(),
            holder,
            last_activity_unix,
            lease_secs: 900,
            host: None,
            pid: None,
            cwd: None,
            model: None,
            title: None,
            last_line: None,
            blocked: false,
            detail_reason: None,
            touched_unix,
        }
    }

    #[test]
    fn record_then_snapshot_round_trips_and_derives_state() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(row("a", Holder::Agent, now, now), now);
        let snap = reg.snapshot(now);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].1, LiveState::Working);
        assert_eq!(snap[0].2, Confidence::Observed);
    }

    #[test]
    fn a_second_beat_for_the_same_session_replaces_not_duplicates() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(row("a", Holder::Agent, now, now), now);
        reg.record_beat(row("a", Holder::Human, now, now), now + 5);
        assert_eq!(reg.len(), 1);
        let snap = reg.snapshot(now + 5);
        assert_eq!(snap[0].0.holder, Holder::Human);
    }

    #[test]
    fn record_beat_reports_the_previous_state_for_transition_detection() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        // First-ever beat: no previous state.
        let first = reg.record_beat(row("a", Holder::Agent, now, now), now);
        assert_eq!(first.previous_state, None);
        assert_eq!(first.state, LiveState::Working);

        // turn_end flips the holder to Human — a real transition
        // (Working -> Waiting).
        let second = reg.record_beat(row("a", Holder::Human, now + 1, now + 1), now + 1);
        assert_eq!(second.previous_state, Some(LiveState::Working));
        assert_eq!(second.state, LiveState::Waiting);
    }

    #[test]
    fn record_beat_with_no_real_change_still_reports_the_same_previous_state() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(row("a", Holder::Agent, now, now), now);
        // A second `tool` beat: still Agent, still Working — the route
        // handler is expected to compare previous_state == state and skip
        // the SSE emit.
        let second = reg.record_beat(row("a", Holder::Agent, now + 1, now + 1), now + 1);
        assert_eq!(second.previous_state, Some(LiveState::Working));
        assert_eq!(second.state, LiveState::Working);
    }

    #[test]
    fn ttl_sweep_evicts_a_row_untouched_past_the_ttl_on_the_next_insert() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(row("stale", Holder::Agent, now, now), now);
        // A second session's beat arrives long after the TTL window for
        // "stale" — the sweep (which runs on every write) must drop it.
        let later = now + REGISTRY_TTL_SECS + 1;
        reg.record_beat(row("fresh", Holder::Agent, later, later), later);
        assert_eq!(reg.len(), 1);
        let snap = reg.snapshot(later);
        assert_eq!(snap[0].0.session_id, "fresh");
    }

    #[test]
    fn cap_eviction_drops_the_least_recently_touched_row_for_a_new_key() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        for i in 0..REGISTRY_CAP {
            let t = now + i as i64;
            reg.record_beat(row(&format!("s{i}"), Holder::Agent, t, t), t);
        }
        assert_eq!(reg.len(), REGISTRY_CAP);
        // One more NEW session must evict "s0" (the oldest touched_unix),
        // not grow past the cap.
        let t = now + REGISTRY_CAP as i64;
        reg.record_beat(row("new", Holder::Agent, t, t), t);
        assert_eq!(reg.len(), REGISTRY_CAP);
        let ids = reg.known_session_ids();
        assert!(!ids.contains("s0"), "oldest-touched row should be evicted");
        assert!(ids.contains("new"));
    }

    #[test]
    fn updating_an_existing_key_never_triggers_cap_eviction() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        for i in 0..REGISTRY_CAP {
            let t = now + i as i64;
            reg.record_beat(row(&format!("s{i}"), Holder::Agent, t, t), t);
        }
        assert_eq!(reg.len(), REGISTRY_CAP);
        // Re-beating an EXISTING session at full capacity must not evict
        // anything else.
        let t = now + REGISTRY_CAP as i64;
        reg.record_beat(row("s0", Holder::Human, t, t), t);
        assert_eq!(reg.len(), REGISTRY_CAP);
        assert!(reg.known_session_ids().contains("s0"));
    }

    #[test]
    fn known_session_ids_reflects_current_membership() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(row("a", Holder::Agent, now, now), now);
        reg.record_beat(row("b", Holder::Human, now, now), now);
        let ids = reg.known_session_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains("a") && ids.contains("b"));
    }
}
