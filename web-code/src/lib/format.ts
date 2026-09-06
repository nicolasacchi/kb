// Small formatting helpers shared by the provenance/annotations/session-diff
// UI — kept pure + colocated with the rest of `lib/` (no component owns
// these outright). Two units, matching the two Rust conventions in play:
// git-side timestamps (`author_time`, `first_seen`, `started_at`, …) are
// unix SECONDS; transcript-side timestamps (`ParsedTurn.ts`, and everything
// derived from it — `Segment.ts`/`UncommittedTurnOut.ts`) are unix
// MILLISECONDS (`transcripts::parse::ParsedTurn`'s own doc).

export function formatUnixSeconds(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}

export function formatUnixMillis(ts: number): string {
  return new Date(ts).toLocaleString();
}

export function shortSha(sha: string, len = 7): string {
  return sha.slice(0, len);
}

/// `{divide duration by this to reach the NEXT unit, this unit's name}` —
/// the standard "divide and conquer" relative-time recipe (each step's
/// `amount` is how many of the CURRENT unit make one of the next):
/// 60 seconds/minute, 60 minutes/hour, 24 hours/day, 7 days/week, ~4.345
/// weeks/month, 12 months/year, and years never roll over further.
const RELATIVE_DIVISIONS: Array<{ amount: number; unit: string }> = [
  { amount: 60, unit: "second" },
  { amount: 60, unit: "minute" },
  { amount: 24, unit: "hour" },
  { amount: 7, unit: "day" },
  { amount: 4.345, unit: "week" },
  { amount: 12, unit: "month" },
  { amount: Number.POSITIVE_INFINITY, unit: "year" },
];

/// A coarse, dependency-free "N units ago" string (Wave C's History
/// inspector tab wants relative time, unlike every other provenance
/// surface's absolute `formatUnixSeconds`). `now` is injectable for
/// deterministic tests; defaults to the real clock. A time in the future
/// (negative delta, e.g. clock skew) degrades to "just now" rather than a
/// nonsensical "-5 minutes ago".
export function relativeTime(unixSeconds: number, now: number = Date.now()): string {
  let duration = Math.floor(now / 1000) - unixSeconds;
  if (duration < 5) return "just now";
  for (const division of RELATIVE_DIVISIONS) {
    if (duration < division.amount) {
      const n = Math.round(duration);
      return `${n} ${division.unit}${n === 1 ? "" : "s"} ago`;
    }
    duration = duration / division.amount;
  }
  // Unreachable — the last division's `amount` is `Infinity`.
  return "a long time ago";
}
