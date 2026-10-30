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
import { relativeAge } from "./time";

export type LiveLanes = {
  /// OL IDEA1 — agent rows blocked on the operator (a permission prompt or
  /// a question): a `blocked` row in working|stalled lives HERE and only
  /// here, longest-blocked first. Same lane-as-presentation rule as the
  /// other three: no new state, the flag on an agent-held row.
  blocked: LiveStatusRow[];
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
  const blocked: LiveStatusRow[] = [];
  const inProgress: LiveStatusRow[] = [];
  const waiting: LiveStatusRow[] = [];
  const finished: LiveStatusRow[] = [];
  for (const r of rows) {
    switch (r.state) {
      case "working":
      case "stalled":
        // A blocked row is the operator's to unblock, not "in progress".
        // `presumed_ended` stays in finished below: 8 h of silence ends
        // the claim, as for any agent row.
        if (r.blocked === true) blocked.push(r);
        else inProgress.push(r);
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
  // Longest-blocked first (the oldest block is the one costing the most).
  blocked.sort((a, b) => blockedSince(a) - blockedSince(b));
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
    blocked,
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

/// Unix seconds a row has been blocked since: the beat that raised the
/// flag, else the row's own last-activity instant (an older daemon sends no
/// `blocked_since_unix`).
export function blockedSince(
  row: Pick<LiveStatusRow, "blocked_since_unix" | "since_unix">,
): number {
  return row.blocked_since_unix ?? row.since_unix;
}

/// How many sessions are blocked on the operator right now — the Header
/// chip and the "Needs you" lane read the same `bucketLiveSessions` output.
export function blockedCount(rows: readonly LiveStatusRow[]): number {
  return bucketLiveSessions(rows).blocked.length;
}

/// The one-line "what is it waiting for": `permission: <tool>` when the hook
/// named one, else Claude's own (trimmed) reason text, else `needs you`.
export function blockedLabel(
  row: Pick<LiveStatusRow, "blocked_tool" | "detail_reason">,
): string {
  if (row.blocked_tool) return `permission: ${row.blocked_tool}`;
  const reason = row.detail_reason?.trim();
  return reason ? reason : "needs you";
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

/// OL IDEA4 — the tooltip for the `last:` evidence chip. Started is not
/// running: kb hears tool STARTS, never ends.
export const LAST_TOOL_TITLE =
  "The most recent tool this session started that kb heard about (name only, never its arguments). Started is not running: the process may have died mid-call.";

/// `last: Bash · 38m` — the name of the last tool the session started and
/// how long ago, or `null` when either wire field is missing. Display only:
/// bucketing and ordering above never read these fields.
export function lastToolLabel(
  row: Pick<LiveStatusRow, "last_tool" | "last_tool_at">,
  nowMs: number,
): string | null {
  if (!row.last_tool || row.last_tool_at == null) return null;
  return `last: ${row.last_tool} · ${relativeAge(row.last_tool_at, nowMs)}`;
}
