// W5/R9b + LF-1 Tier 0 — the derived-never-stored session presence/
// staleness signal. Pure function over (row, now) — NO server probe (that's
// Tier 1's `GET /api/sessions/presence`, W7): every deployment, including
// prod/phone with no view of `~/.claude/projects`, gets this floor for free
// from data already on `SessionRow`.
//
// Mechanism (memo R9c/M3, sharpened by D2 + LF-1): `ended_at` on a mid-run
// (throttled, `kb-capture-throttle.sh`) capture is already the max EVENT
// timestamp the capture saw — not just a Stop — so `now - ended_at` is
// honest at whatever cadence captures actually land at (Stop-only without
// D2, ~`KB_CAPTURE_MIN_INTERVAL_SECS` with D2 on). The copy NEVER implies
// file-level liveness (that dishonesty is exactly what Tier 1 exists to fix
// later, W7) — it always reads "as of capture", the M3/LF-1-ratified
// wording, so every surface stays truthful about its own granularity.

// W7/LF-1 amendment: a second input, `presenceSet` (the Tier-1
// `GET /api/sessions/presence` probe's live session-id set, joined
// client-side against list rows by `session_id`) upgrades "active" to
// "live" — the ONE place Tier-0 and Tier-1 reconcile, per LF-1's "ONE
// presence surface" rule. Tier-1 is EVIDENCE-based (a writer touched the
// file within `live_window_secs`), so unlike Tier-0's honestly-hedged
// "active" it earns the pulsing dot (list-row/presence-strip/context-card
// chip wiring all key off `status === "live"` for that, never "active").

import { relativeAge } from "./time";

/// M3's ratified default: a session whose newest capture landed within the
/// last 10 minutes reads "active". `kb_core::sessions` has no server-side
/// twin of this constant — it is deliberately SPA-only derived state (#25
/// precedent: computed per response, never persisted).
export const ACTIVE_WINDOW_SECS = 600;

export type SessionPresenceStatus = "live" | "active" | "idle";

export interface SessionPresence {
  status: SessionPresenceStatus;
  /// Honest copy, tier-labelled: "live — writing now" (Tier-1, evidence) /
  /// "active — as of capture · 3m ago" (Tier-0) / "as of capture · 2h ago".
  /// Never a spinner, and Tier-0 copy never implies file-level liveness.
  copy: string;
}

function agoPhrase(unixSeconds: number, now: number): string {
  const age = relativeAge(unixSeconds, now);
  return age === "now" ? "just now" : `${age} ago`;
}

/// The Tier-1 presence set the daemon's `/api/sessions/presence` returns,
/// pre-joined to a plain `Set<session_id>` (`presenceLiveSet` below) so
/// `sessionPresence` itself stays a synchronous, allocation-cheap pure
/// function — it never touches the wire response shape directly.
export type PresenceLiveSet = ReadonlySet<string>;

const EMPTY_LIVE_SET: PresenceLiveSet = new Set();

/// Build the client-side join set from the raw wire response — pure,
/// exported so `useSessionPresence` (the polling hook) and tests share one
/// derivation.
export function presenceLiveSet(
  live: readonly { session_id: string }[] | undefined,
): PresenceLiveSet {
  if (!live || live.length === 0) return EMPTY_LIVE_SET;
  return new Set(live.map((e) => e.session_id));
}

/// The pure derivation: `(row, now, presenceSet?) -> "live"|"active"|"idle"`
/// (LF-1's literal grammar — Tier 1 strictly upgrades Tier 0, never
/// downgrades it), plus the honest, tier-labelled copy string. `now` is
/// injectable for deterministic tests; defaults to `Date.now()`.
/// `presenceSet` defaults to empty (Tier-1 unavailable/unconfigured) so
/// every pre-W7 call site — which never passes it — is byte-unchanged.
export function sessionPresence(
  row: { session_id?: string; ended_at: number },
  now: number = Date.now(),
  presenceSet: PresenceLiveSet = EMPTY_LIVE_SET,
): SessionPresence {
  const nowSecs = Math.floor(now / 1000);
  const ageSecs = Math.max(0, nowSecs - row.ended_at);
  const tier0Active = ageSecs < ACTIVE_WINDOW_SECS;
  const isLive = !!row.session_id && presenceSet.has(row.session_id);
  const status: SessionPresenceStatus = isLive
    ? "live"
    : tier0Active
      ? "active"
      : "idle";
  if (status === "live") {
    return { status, copy: "live — writing now" };
  }
  const phrase = agoPhrase(row.ended_at, now);
  const copy =
    status === "active"
      ? `active — as of capture · ${phrase}`
      : `as of capture · ${phrase}`;
  return { status, copy };
}

/// Just the status, for callers that only need the dot/badge (no tooltip).
export function sessionPresenceStatus(
  row: { session_id?: string; ended_at: number },
  now: number = Date.now(),
  presenceSet: PresenceLiveSet = EMPTY_LIVE_SET,
): SessionPresenceStatus {
  return sessionPresence(row, now, presenceSet).status;
}

/// Derive presence summary counts from a list of rows: "N live · M active"
/// (LF-1/S2's "PRESENCE STRIP counts"). Call once over the visible rows +
/// presenceSet to build the strip label and project-home card chips.
/// Pure derivation, deterministic, testable.
export interface PresenceSummary {
  live: number;
  active: number;
}

export function presenceSummary(
  rows: readonly { session_id?: string; ended_at: number }[],
  now: number = Date.now(),
  presenceSet: PresenceLiveSet = EMPTY_LIVE_SET,
): PresenceSummary {
  let live = 0;
  let active = 0;
  for (const row of rows) {
    const status = sessionPresenceStatus(row, now, presenceSet);
    if (status === "live") live++;
    else if (status === "active") active++;
  }
  return { live, active };
}

/// Format the presence summary as "N live · M active" for display. Omits
/// either count when zero (e.g., "5 live" with no active rows, or "3 active"
/// with no live rows).
export function formatPresenceSummary(summary: PresenceSummary): string {
  const parts = [];
  if (summary.live > 0) parts.push(`${summary.live} live`);
  if (summary.active > 0) parts.push(`${summary.active} active`);
  return parts.join(" · ") || "idle";
}
