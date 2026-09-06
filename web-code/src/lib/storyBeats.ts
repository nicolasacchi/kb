// CT-E2 — pure display logic for the story timeline's attention-gap beat
// (`provenance::story`'s `status: "gap"` entries: a run of consecutive
// commits the join ladder resolved NO session for, collapsed SERVER-side
// into one beat). The SPA never re-aggregates — the server owns the
// collapse; this module only decides how a beat READS. Kept DOM/router-free
// and colocated with the rest of `lib/` so vitest covers it directly.

import type { StoryEntry } from "../api/types";

/// `true` for a CT-E2 attention-gap beat — the one entry kind with no
/// session identity to render as a normal timeline row.
export function isGapBeat(e: StoryEntry): boolean {
  return e.status === "gap";
}

/// UTC calendar date (`YYYY-MM-DD`) — deliberately locale-independent
/// (unlike `format.ts`'s `formatUnixSeconds`): the gap divider is a compact
/// RANGE label where two locale-formatted datetimes would wrap, and an ISO
/// date is deterministic under vitest's node environment regardless of ICU
/// locale.
export function gapDate(unixSeconds: number): string {
  return new Date(unixSeconds * 1000).toISOString().slice(0, 10);
}

/// The gap divider's copy — count + date range, honest about WHICH claim
/// the server could back (`reason`): `"no-captured-session"` reads as a
/// definite "no captured session" (kb answered, nothing matched);
/// `"join-unavailable"` — or an absent/unknown reason, fail-honest — reads
/// as "session join unavailable" (coverage unknown, not known-absent).
export function gapBeatLabel(e: StoryEntry): string {
  const count = e.commit_count ?? 1;
  const what =
    e.reason === "no-captured-session" ? "no captured session" : "session join unavailable";
  const from = gapDate(e.first_seen);
  const to = gapDate(e.last_seen ?? e.first_seen);
  const range = from === to ? from : `${from}..${to}`;
  return `${what} for ${count} commit${count === 1 ? "" : "s"} (${range})`;
}
