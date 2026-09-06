// LSC-4 — the live-sessions cockpit's pure bucketing/ordering/honesty logic
// (design docs/research/kb-live-sessions-cockpit-2026-08.html §3 "two axes,
// not three buckets" + §7 "the human interface"). Kept separate from
// sessions.tsx (already ~2000 lines) so the logic gets direct unit coverage
// without mounting the whole route — the `CodeRefsSection.tsx` pure-helper
// precedent (DCB W1.D.R #3).
//
// kb-core's `derive_state` returns SIX `LiveState` values; the operator
// asked for THREE lanes. Design §3 is explicit that the three lanes are a
// RENDERING of the "who holds the ball" axis — the second axis (how long
// since it moved) never flips a lane, it only decorates:
//
//   working, stalled          -> IN PROGRESS. A `stalled` row stays here
//                                with a doubt marker, never demoted to
//                                waiting — "the transcript says the agent
//                                has the turn, and silence is not evidence
//                                to the contrary" (§3).
//   waiting, cold              -> WAITING ON YOU. `cold` is the SAME axis
//                                (human still holds the ball) just older
//                                than a working day's patience — folding it
//                                into this lane (rather than hiding it, or
//                                inventing a fourth lane the operator never
//                                asked for) keeps "longest wait first"
//                                honest: a session that's sat on the
//                                operator for 9 hours IS the "you are the
//                                bottleneck" case, not one to bury.
//   finished, presumed_ended   -> FINISHED. `presumed_ended` is an
//                                INFERENCE (no end signal fired; the lease
//                                just expired with nothing landing) and
//                                must never read as an observed end — see
//                                `liveHonestyKind` below, which every
//                                presumed_ended row also trips (design §6:
//                                "source"/"confidence" are the mechanism by
//                                which this feature avoids the earlier
//                                prototype's central failure").

import type { LiveStatusRow } from "../api/sessions";

export type LiveLanes = {
  inProgress: LiveStatusRow[];
  waiting: LiveStatusRow[];
  /// Capped at `cap` (default `FINISHED_LANE_CAP`) — the day-buckets below
  /// the Now band already list full history; this lane is "what just
  /// happened", not a second history view.
  finished: LiveStatusRow[];
  /// The finished/presumed_ended count BEFORE capping, so the UI can say
  /// "showing N of M" instead of silently truncating.
  finishedTotal: number;
};

/// The Now band deliberately does not dump every cold-finished session —
/// the day-buckets below already list full history (task brief, "do not
/// dump every cold session here … cap it, and say in the UI that it is
/// capped").
export const FINISHED_LANE_CAP = 5;

/// Bucket + order a `live-status` row set into the three Now-band lanes.
/// Pure, deterministic, `now`-independent (ordering is by `since_unix`, not
/// by a live-ticking age) — the elapsed-timer DISPLAY is a client concern
/// (`useNowTick`), never a reason to re-sort.
export function bucketLiveSessions(
  rows: readonly LiveStatusRow[],
  cap: number = FINISHED_LANE_CAP,
): LiveLanes {
  const inProgress: LiveStatusRow[] = [];
  const waiting: LiveStatusRow[] = [];
  const finished: LiveStatusRow[] = [];
  for (const r of rows) {
    switch (r.state) {
      case "working":
      case "stalled":
        inProgress.push(r);
        break;
      case "waiting":
      case "cold":
        waiting.push(r);
        break;
      case "finished":
      case "presumed_ended":
        finished.push(r);
        break;
    }
  }
  // Most-recently-active first: largest since_unix (the most recent
  // instant) sorts first.
  inProgress.sort((a, b) => b.since_unix - a.since_unix);
  // Longest-wait-first: smallest since_unix (the OLDEST instant — "you've
  // been sitting on this the longest") sorts first. Design §7: "that is the
  // 'you are the bottleneck' signal."
  waiting.sort((a, b) => a.since_unix - b.since_unix);
  // Newest-first.
  finished.sort((a, b) => b.since_unix - a.since_unix);
  return {
    inProgress,
    waiting,
    finished: finished.slice(0, cap),
    finishedTotal: finished.length,
  };
}

/// How many sessions are waiting on the operator right now — the Header
/// chip's count and the WAITING lane's row count are the SAME number by
/// construction (both read off `bucketLiveSessions`; never a second,
/// independently-computed count that could disagree with the lane itself).
export function waitingCount(rows: readonly LiveStatusRow[]): number {
  return bucketLiveSessions(rows).waiting.length;
}

/// CT-E3's honest-staleness idiom (`docs/architecture-invariants.md` §CT-E3
/// precedent, `CodeRefsSection.tsx`'s `CodeFreshness`), applied to a live
/// row: `presumed` confidence, or a `capture`-sourced row (Tier-0 — a
/// daemon-restart rebuild from landed captures, design §6), MUST be
/// visually marked, never rendered indistinguishably from a hook-observed
/// row. `null` for an ordinary observed/hook row — no badge, nothing to
/// say.
export type LiveHonestyKind = "presumed" | "capture";

export function liveHonestyKind(
  row: Pick<LiveStatusRow, "confidence" | "source">,
): LiveHonestyKind | null {
  // `presumed` is the stronger caveat (the daemon is guessing at the STATE
  // itself, not just working from a slower signal) — a row can be both
  // capture-sourced AND presumed, so check it first.
  if (row.confidence === "presumed") return "presumed";
  if (row.source === "capture") return "capture";
  return null;
}

export function liveHonestyLabel(kind: LiveHonestyKind): string {
  return kind === "presumed" ? "presumed" : "as of last capture";
}

export function liveHonestyTitle(kind: LiveHonestyKind): string {
  return kind === "presumed"
    ? "presumed — no beat or capture confirms this state yet (a fresh daemon boot, or the lease expired with nothing landing since)"
    : "learned from the last landed capture, not a live signal — the daemon has no beat on record for this session";
}

/// `cold` decorates a WAITING row (folded into that lane above) with an
/// honest "this has sat a while" marker — distinct from the presumed/
/// capture source-honesty badge above, which is about HOW we know, not how
/// LONG it's been true.
export function isLongWait(row: Pick<LiveStatusRow, "state">): boolean {
  return row.state === "cold";
}

/// `stalled` decorates an IN-PROGRESS row: the agent still holds the turn
/// (never demoted to waiting — design §3), but its lease is well past any
/// plausible single tool call. A doubt marker, not a state change.
export function isStalled(row: Pick<LiveStatusRow, "state">): boolean {
  return row.state === "stalled";
}

/// `presumed_ended` decorates a FINISHED row: no `SessionEnd`/end-of-turn
/// signal fired — the lease simply expired with nothing landing. An
/// inference, never rendered as an observed end.
export function isPresumedEnded(row: Pick<LiveStatusRow, "state">): boolean {
  return row.state === "presumed_ended";
}
