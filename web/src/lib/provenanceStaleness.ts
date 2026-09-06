// MI-W4.6 — pure staleness formatting for the provenance thread's 3rd hop
// (memory → session → commits → "has the touched file changed since").
// No React, no fetch — colocated `provenanceStaleness.test.ts` covers it
// with plain vitest, same idiom as `decayProjection.ts`.

/// Whole days between `thenUnix` and `nowUnix`, clamped at 0 (never
/// negative — a clock skew or a same-second read reads as "today", not a
/// negative day count).
export function daysAgo(thenUnix: number, nowUnix: number): number {
  return Math.max(0, Math.floor((nowUnix - thenUnix) / 86400));
}

/// "today" / "1d ago" / "Nd ago" for a known unix-seconds timestamp.
export function daysAgoLabel(thenUnix: number, nowUnix: number): string {
  const days = daysAgo(thenUnix, nowUnix);
  if (days === 0) return "today";
  if (days === 1) return "1d ago";
  return `${days}d ago`;
}

/// The one-line staleness verdict for a `TouchedFileOut`-shaped row —
/// `changed_since`/`last_touched_unix` are BOTH `None` together (an
/// unresolvable git lookup) or both present (`commit_touched_files` never
/// splits them), so this is total over the pair, not per-field.
export function fileStalenessLabel(
  file: { last_touched_unix?: number | null; changed_since?: boolean | null },
  nowUnix: number,
): string {
  if (file.changed_since == null || file.last_touched_unix == null) {
    return "unknown — git lookup unavailable";
  }
  const age = daysAgoLabel(file.last_touched_unix, nowUnix);
  return file.changed_since ? `changed again ${age}` : `unchanged since this commit (${age})`;
}
