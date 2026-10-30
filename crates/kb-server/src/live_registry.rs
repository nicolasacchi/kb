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
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

/// Per-session cap on remembered touches (OL PRES1). The ring is in memory
/// only (LF-7) and dies with its row.
pub const TOUCH_RING_CAP: usize = 48;

/// A touch older than this (receipt clock) is pruned on the next beat for
/// its session and filtered out of every read.
pub const TOUCH_TTL_SECS: i64 = 3600;

/// One remembered write-tool touch: a path RELATIVE to the session's repo
/// root (the daemon never stores the absolute path or the root), the project
/// label the beat carried, the receipt-clock instant and the tool name.
/// `worktree` is the BASENAME of a linked worktree's directory (OL PRES2),
/// `None` for the main checkout; it is part of a touch's identity, so the
/// same `rel` in two worktrees of one project are two touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touch {
    pub project: String,
    pub worktree: Option<String>,
    pub rel: String,
    pub at_unix: i64,
    pub tool: String,
}

/// What [`LiveRegistry::touches_snapshot`] hands the live-touches route: the
/// derived state plus a clone of the (already TTL-filtered) ring. Narrow on
/// purpose: no cwd, title, last_line or model can reach the route.
#[derive(Debug, Clone)]
pub struct TouchSnapshot {
    pub session_id: String,
    pub harness: String,
    pub state: LiveState,
    pub touches: Vec<Touch>,
}

/// OL IDEA4 — the newest tool a session STARTED (a `tool_label`, never
/// arguments) and the beat `at` that said so. In memory only (LF-7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastTool {
    pub name: String,
    pub at_unix: i64,
}

/// What one beat does to a row's [`LastTool`]. Input only: the stored row
/// always carries `Keep`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolUpdate {
    /// Leave the remembered tool as it was (heartbeats, blocked, ...).
    #[default]
    Keep,
    /// A turn boundary at this beat `at`: forget the tool.
    Clear { at_unix: i64 },
    /// A tool-start beat.
    Started(LastTool),
}

/// Apply a [`ToolUpdate`] to the previous tool. Monotonic: a late-arriving
/// older beat never wins (a start or clear with `at` older than the
/// remembered tool's keeps it; on a tie the newer arrival wins).
pub fn next_last_tool(prev: Option<&LastTool>, update: &ToolUpdate) -> Option<LastTool> {
    match update {
        ToolUpdate::Keep => prev.cloned(),
        ToolUpdate::Started(t) => match prev {
            Some(p) if t.at_unix < p.at_unix => Some(p.clone()),
            _ => Some(t.clone()),
        },
        ToolUpdate::Clear { at_unix } => match prev {
            Some(p) if *at_unix < p.at_unix => Some(p.clone()),
            _ => None,
        },
    }
}

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
    /// The tool a `blocked` beat named (`detail.tool`), validated at intake;
    /// only ever set on a blocked row.
    pub blocked_tool: Option<String>,
    /// Unix seconds of the `blocked` beat that set the flag (that beat's own
    /// `at`); only ever set on a blocked row.
    pub blocked_since_unix: Option<i64>,
    /// Wall-clock receipt time (unix seconds) — the TTL/LRU-by-touch clock.
    /// Distinct from `last_activity_unix`; see that field's doc comment.
    pub touched_unix: i64,
    /// On input to [`LiveRegistry::record_beat`]: the touches THIS beat
    /// carried. On the stored row (and the returned [`RecordOutcome::row`]):
    /// the merged ring, oldest first, at most [`TOUCH_RING_CAP`].
    pub touches: VecDeque<Touch>,
    /// OL IDEA4 — on input: what this beat does to the remembered tool.
    /// Always `Keep` on the stored row and the returned row.
    pub tool_update: ToolUpdate,
    /// OL IDEA4 — stored/computed: the newest started tool, merged by
    /// [`next_last_tool`] under the registry lock. Ignores any value the
    /// caller put here; display evidence only, never a `derive_state` input.
    pub last_tool: Option<LastTool>,
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
    /// The `blocked` flag the session carried immediately BEFORE this beat
    /// (`None` for a session's first-ever beat).
    pub previous_blocked: Option<bool>,
}

/// What a beat does to the sticky `blocked` flag (OL IDEA1): `blocked` sets
/// it, a `tool` heartbeat keeps whatever the row had, every other event
/// clears it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockEffect {
    Set,
    Keep,
    Clear,
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
        let effect = if row.blocked {
            BlockEffect::Set
        } else {
            BlockEffect::Clear
        };
        self.record_beat_effect(row, now_unix, effect)
    }

    /// [`Self::record_beat`] with an explicit effect on the sticky blocked
    /// flag, applied under the same lock as the touch-ring carry-forward.
    pub fn record_beat_effect(
        &self,
        mut row: LiveRow,
        now_unix: i64,
        effect: BlockEffect,
    ) -> RecordOutcome {
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

        let previous_blocked = g.get(&row.session_id).map(|old| old.blocked);

        // Blocked flag (OL IDEA1), resolved under this one lock: Set takes
        // this beat's reason/tool and stamps `since` with the beat's own
        // `at` (Claude sends one notification per prompt, so a second blocked
        // beat is a new prompt); Keep carries a blocked old row's four fields
        // forward; Clear drops them all.
        match effect {
            BlockEffect::Set => {
                row.blocked = true;
                row.blocked_since_unix = Some(row.last_activity_unix);
            }
            BlockEffect::Keep => match g.get(&row.session_id).filter(|old| old.blocked) {
                Some(old) => {
                    row.blocked = true;
                    row.detail_reason = old.detail_reason.clone();
                    row.blocked_tool = old.blocked_tool.clone();
                    row.blocked_since_unix = old.blocked_since_unix;
                }
                None => {
                    row.blocked = false;
                    row.detail_reason = None;
                    row.blocked_tool = None;
                    row.blocked_since_unix = None;
                }
            },
            BlockEffect::Clear => {
                row.blocked = false;
                row.detail_reason = None;
                row.blocked_tool = None;
                row.blocked_since_unix = None;
            }
        }

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

        // Touch ring (OL PRES1): carry the old ring forward under this same
        // lock, drop expired entries, then merge this beat's touches —
        // a repeat of (project, worktree, rel) moves to the back with its fresh
        // instant, and the oldest entry is evicted past the cap. An `end`
        // beat carries no touches and so leaves the ring intact.
        let mut ring: VecDeque<Touch> = g
            .get(&row.session_id)
            .map(|old| old.touches.clone())
            .unwrap_or_default();
        ring.retain(|t| now_unix - t.at_unix <= TOUCH_TTL_SECS);
        for t in std::mem::take(&mut row.touches) {
            if let Some(pos) = ring
                .iter()
                .position(|x| x.project == t.project && x.worktree == t.worktree && x.rel == t.rel)
            {
                ring.remove(pos);
            }
            ring.push_back(t);
            while ring.len() > TOUCH_RING_CAP {
                ring.pop_front();
            }
        }
        row.touches = ring;

        // Last started tool (OL IDEA4): merged under this same lock, from the
        // stored row's value and this beat's update; no `.await` (#15).
        row.last_tool = next_last_tool(
            g.get(&row.session_id)
                .and_then(|old| old.last_tool.as_ref()),
            &std::mem::take(&mut row.tool_update),
        );

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
            previous_blocked,
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

    /// Every session's non-expired touches with its freshly-derived state.
    /// The guard is dropped before this returns (invariant #15): the route
    /// filters, sorts and serialises the owned clones.
    pub fn touches_snapshot(&self, now_unix: i64) -> Vec<TouchSnapshot> {
        let policy = LivePolicy::default();
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let out = g
            .values()
            .filter(|r| !r.touches.is_empty())
            .map(|r| TouchSnapshot {
                session_id: r.session_id.clone(),
                harness: r.harness.clone(),
                state: derive_state(
                    r.holder,
                    r.last_activity_unix,
                    now_unix,
                    kb_core::sessions::live::StateSource::Hook,
                    &policy,
                )
                .0,
                touches: r
                    .touches
                    .iter()
                    .filter(|t| now_unix - t.at_unix <= TOUCH_TTL_SECS)
                    .cloned()
                    .collect(),
            })
            .collect();
        drop(g);
        out
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
            blocked_tool: None,
            blocked_since_unix: None,
            touched_unix,
            touches: VecDeque::new(),
            tool_update: ToolUpdate::Keep,
            last_tool: None,
        }
    }

    fn lt(name: &str, at: i64) -> LastTool {
        LastTool {
            name: name.to_string(),
            at_unix: at,
        }
    }

    fn with_update(mut r: LiveRow, u: ToolUpdate) -> LiveRow {
        r.tool_update = u;
        r
    }

    fn touch(rel: &str, at: i64) -> Touch {
        Touch {
            project: "demo".to_string(),
            worktree: None,
            rel: rel.to_string(),
            at_unix: at,
            tool: "Edit".to_string(),
        }
    }

    fn with_touches(mut r: LiveRow, ts: Vec<Touch>) -> LiveRow {
        r.touches = ts.into();
        r
    }

    fn rels(reg: &LiveRegistry, now: i64) -> Vec<String> {
        reg.touches_snapshot(now)
            .into_iter()
            .flat_map(|s| s.touches.into_iter().map(|t| t.rel))
            .collect()
    }

    fn blocked_row(sid: &str, at: i64) -> LiveRow {
        let mut r = row(sid, Holder::Agent, at, at);
        r.blocked = true;
        r.detail_reason = Some("Claude needs your permission to use Bash".to_string());
        r.blocked_tool = Some("Bash".to_string());
        r
    }

    fn stored(reg: &LiveRegistry, now: i64) -> LiveRow {
        reg.snapshot(now).remove(0).0
    }

    #[test]
    fn set_then_keep_carries_all_four_blocked_fields() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat_effect(blocked_row("a", now), now, BlockEffect::Set);
        let out = reg.record_beat_effect(
            row("a", Holder::Agent, now + 30, now + 30),
            now + 30,
            BlockEffect::Keep,
        );
        assert_eq!(out.previous_blocked, Some(true));
        let r = stored(&reg, now + 30);
        assert!(r.blocked);
        assert_eq!(r.blocked_tool.as_deref(), Some("Bash"));
        assert_eq!(
            r.detail_reason.as_deref(),
            Some("Claude needs your permission to use Bash")
        );
        assert_eq!(r.blocked_since_unix, Some(now));
    }

    #[test]
    fn set_then_clear_drops_the_blocked_fields() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat_effect(blocked_row("a", now), now, BlockEffect::Set);
        reg.record_beat(row("a", Holder::Agent, now + 5, now + 5), now + 5);
        let r = stored(&reg, now + 5);
        assert!(!r.blocked);
        assert!(r.detail_reason.is_none() && r.blocked_tool.is_none());
        assert!(r.blocked_since_unix.is_none());
    }

    #[test]
    fn set_then_turn_end_clears_and_the_holder_is_human() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat_effect(blocked_row("a", now), now, BlockEffect::Set);
        let out = reg.record_beat_effect(
            row("a", Holder::Human, now + 5, now + 5),
            now + 5,
            BlockEffect::Clear,
        );
        assert_eq!(out.state, LiveState::Waiting);
        let r = stored(&reg, now + 5);
        assert!(!r.blocked && r.blocked_since_unix.is_none());
    }

    #[test]
    fn keep_with_no_prior_blocked_row_is_not_blocked() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        let first =
            reg.record_beat_effect(row("a", Holder::Agent, now, now), now, BlockEffect::Keep);
        assert_eq!(first.previous_blocked, None);
        assert!(!stored(&reg, now).blocked);
        reg.record_beat_effect(
            row("a", Holder::Agent, now + 1, now + 1),
            now + 1,
            BlockEffect::Keep,
        );
        assert!(!stored(&reg, now + 1).blocked);
    }

    #[test]
    fn a_second_set_resets_since_to_the_new_beat() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat_effect(blocked_row("a", now), now, BlockEffect::Set);
        reg.record_beat_effect(blocked_row("a", now + 90), now + 90, BlockEffect::Set);
        assert_eq!(stored(&reg, now + 90).blocked_since_unix, Some(now + 90));
    }

    #[test]
    fn next_last_tool_starts_keeps_and_clears() {
        let a = lt("Bash", 100);
        assert_eq!(next_last_tool(None, &ToolUpdate::Keep), None);
        assert_eq!(next_last_tool(Some(&a), &ToolUpdate::Keep), Some(a.clone()));
        assert_eq!(
            next_last_tool(None, &ToolUpdate::Started(a.clone())),
            Some(a.clone())
        );
        let b = lt("Read", 200);
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Started(b.clone())),
            Some(b)
        );
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Clear { at_unix: 150 }),
            None
        );
        assert_eq!(
            next_last_tool(None, &ToolUpdate::Clear { at_unix: 1 }),
            None
        );
        // A tie: the newer arrival wins, for a start and for a clear.
        let tie = lt("Grep", 100);
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Started(tie.clone())),
            Some(tie)
        );
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Clear { at_unix: 100 }),
            None
        );
    }

    #[test]
    fn next_last_tool_ignores_an_older_start_or_clear() {
        let a = lt("Bash", 200);
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Started(lt("Read", 199))),
            Some(a.clone())
        );
        assert_eq!(
            next_last_tool(Some(&a), &ToolUpdate::Clear { at_unix: 199 }),
            Some(a)
        );
    }

    #[test]
    fn record_beat_carries_last_tool_across_heartbeats_and_clears_on_turn_boundaries() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        let out = reg.record_beat(
            with_update(
                row("a", Holder::Agent, now, now),
                ToolUpdate::Started(lt("Bash", now)),
            ),
            now,
        );
        assert_eq!(out.row.last_tool, Some(lt("Bash", now)));
        assert_eq!(out.row.tool_update, ToolUpdate::Keep);
        // A heartbeat (Keep) carries it.
        reg.record_beat(row("a", Holder::Agent, now + 30, now + 30), now + 30);
        assert_eq!(stored(&reg, now + 30).last_tool, Some(lt("Bash", now)));
        assert_eq!(stored(&reg, now + 30).tool_update, ToolUpdate::Keep);
        // A caller-supplied `last_tool` on input is ignored (Keep carries the old).
        let mut forged = row("a", Holder::Agent, now + 40, now + 40);
        forged.last_tool = Some(lt("Forged", now + 40));
        reg.record_beat(forged, now + 40);
        assert_eq!(stored(&reg, now + 40).last_tool, Some(lt("Bash", now)));
        // A newer start replaces it.
        reg.record_beat(
            with_update(
                row("a", Holder::Agent, now + 50, now + 50),
                ToolUpdate::Started(lt("Read", now + 50)),
            ),
            now + 50,
        );
        assert_eq!(stored(&reg, now + 50).last_tool, Some(lt("Read", now + 50)));
        // A turn boundary clears it.
        reg.record_beat(
            with_update(
                row("a", Holder::Human, now + 60, now + 60),
                ToolUpdate::Clear { at_unix: now + 60 },
            ),
            now + 60,
        );
        assert_eq!(stored(&reg, now + 60).last_tool, None);
        // A first-ever Keep beat has no tool.
        reg.record_beat(row("b", Holder::Agent, now, now), now);
        let b = reg
            .snapshot(now)
            .into_iter()
            .find(|(r, _, _)| r.session_id == "b")
            .unwrap()
            .0;
        assert_eq!(b.last_tool, None);
    }

    // invariant:11 tool evidence never changes the derived state
    #[test]
    fn tool_evidence_never_changes_the_derived_state() {
        let with = LiveRegistry::new();
        let without = LiveRegistry::new();
        let t = 1_000_000;
        let seq: Vec<(Holder, i64, ToolUpdate)> = vec![
            (Holder::Agent, t, ToolUpdate::Clear { at_unix: t }),
            (
                Holder::Agent,
                t + 10,
                ToolUpdate::Started(lt("Bash", t + 10)),
            ),
            (Holder::Agent, t + 3000, ToolUpdate::Keep),
            (
                Holder::Agent,
                t + 3010,
                ToolUpdate::Started(lt("Read", t + 3010)),
            ),
            (
                Holder::Human,
                t + 3020,
                ToolUpdate::Clear { at_unix: t + 3020 },
            ),
            (
                Holder::Agent,
                t + 3030,
                ToolUpdate::Started(lt("Edit", t + 3030)),
            ),
            (
                Holder::Ended,
                t + 3040,
                ToolUpdate::Clear { at_unix: t + 3040 },
            ),
        ];
        for (holder, at, upd) in seq {
            let a = with.record_beat(with_update(row("s", holder, at, at), upd), at);
            let b = without.record_beat(row("s", holder, at, at), at);
            assert_eq!(a.state, b.state);
            assert_eq!(a.confidence, b.confidence);
            assert_eq!(a.previous_state, b.previous_state);
            assert_eq!(
                with.snapshot(at + 5000)[0].1,
                without.snapshot(at + 5000)[0].1
            );
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

    #[test]
    fn fresh_registry_has_no_touches() {
        assert!(LiveRegistry::new().touches_snapshot(1_000_000).is_empty());
    }

    #[test]
    fn touch_ring_carries_across_a_path_less_beat() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(
            with_touches(row("a", Holder::Agent, now, now), vec![touch("x.rs", now)]),
            now,
        );
        // A path-less beat (e.g. turn_end) must not clear the ring.
        let out = reg.record_beat(row("a", Holder::Human, now + 5, now + 5), now + 5);
        assert_eq!(out.row.touches.len(), 1);
        assert_eq!(rels(&reg, now + 5), vec!["x.rs"]);
    }

    #[test]
    fn touch_dedupe_updates_the_instant_and_moves_to_the_back() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(
            with_touches(
                row("a", Holder::Agent, now, now),
                vec![touch("a.rs", now), touch("b.rs", now + 1)],
            ),
            now + 1,
        );
        reg.record_beat(
            with_touches(
                row("a", Holder::Agent, now + 9, now + 9),
                vec![touch("a.rs", now + 9)],
            ),
            now + 9,
        );
        let snap = reg.touches_snapshot(now + 9);
        let t = &snap[0].touches;
        assert_eq!(
            t.iter().map(|x| x.rel.as_str()).collect::<Vec<_>>(),
            vec!["b.rs", "a.rs"]
        );
        assert_eq!(t[1].at_unix, now + 9);
    }

    #[test]
    fn same_rel_in_two_worktrees_stays_two_touches() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        let mut wt = touch("a.rs", now);
        wt.worktree = Some("feat".to_string());
        reg.record_beat(
            with_touches(
                row("a", Holder::Agent, now, now),
                vec![touch("a.rs", now), wt],
            ),
            now,
        );
        let mut wt2 = touch("a.rs", now + 3);
        wt2.worktree = Some("feat".to_string());
        reg.record_beat(
            with_touches(row("a", Holder::Agent, now + 3, now + 3), vec![wt2]),
            now + 3,
        );
        let snap = reg.touches_snapshot(now + 3);
        let t = &snap[0].touches;
        assert_eq!(t.len(), 2, "main + worktree, the repeat deduped in place");
        assert_eq!(t[0].worktree, None);
        assert_eq!(t[1].worktree.as_deref(), Some("feat"));
        assert_eq!(t[1].at_unix, now + 3);
    }

    #[test]
    fn touch_ring_cap_evicts_the_oldest() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        let ts: Vec<Touch> = (0..TOUCH_RING_CAP + 2)
            .map(|i| touch(&format!("f{i}.rs"), now))
            .collect();
        reg.record_beat(with_touches(row("a", Holder::Agent, now, now), ts), now);
        let r = rels(&reg, now);
        assert_eq!(r.len(), TOUCH_RING_CAP);
        assert_eq!(r[0], "f2.rs");
        assert_eq!(r[TOUCH_RING_CAP - 1], format!("f{}.rs", TOUCH_RING_CAP + 1));
    }

    #[test]
    fn touches_older_than_3600s_are_pruned() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(
            with_touches(
                row("a", Holder::Agent, now, now),
                vec![touch("old.rs", now)],
            ),
            now,
        );
        let later = now + TOUCH_TTL_SECS + 1;
        // Read-side filter, before any further write.
        assert!(rels(&reg, later).is_empty());
        // Write-side prune: the next beat drops it from the stored ring.
        let out = reg.record_beat(row("a", Holder::Agent, later, later), later);
        assert!(out.row.touches.is_empty());
    }

    #[test]
    fn end_keeps_the_ring() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(
            with_touches(row("a", Holder::Agent, now, now), vec![touch("x.rs", now)]),
            now,
        );
        reg.record_beat(row("a", Holder::Ended, now + 2, now + 2), now + 2);
        let snap = reg.touches_snapshot(now + 2);
        assert_eq!(snap[0].state, LiveState::Finished);
        assert_eq!(rels(&reg, now + 2), vec!["x.rs"]);
    }

    #[test]
    fn ttl_and_lru_eviction_drop_the_ring() {
        let reg = LiveRegistry::new();
        let now = 1_000_000;
        reg.record_beat(
            with_touches(
                row("stale", Holder::Agent, now, now),
                vec![touch("x.rs", now)],
            ),
            now,
        );
        let later = now + REGISTRY_TTL_SECS + 1;
        reg.record_beat(row("fresh", Holder::Agent, later, later), later);
        assert!(reg.touches_snapshot(later).is_empty());

        // LRU: fill to the cap with the first session holding a touch.
        let reg = LiveRegistry::new();
        reg.record_beat(
            with_touches(row("s0", Holder::Agent, now, now), vec![touch("x.rs", now)]),
            now,
        );
        for i in 1..REGISTRY_CAP {
            let t = now + i as i64;
            reg.record_beat(row(&format!("s{i}"), Holder::Agent, t, t), t);
        }
        let t = now + REGISTRY_CAP as i64;
        reg.record_beat(row("new", Holder::Agent, t, t), t);
        assert!(!reg.known_session_ids().contains("s0"));
        assert!(rels(&reg, t).is_empty());
    }
}
