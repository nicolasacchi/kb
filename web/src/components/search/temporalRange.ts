// W1.search — pure date-to-unix helpers for the search page's "read
// during" temporal scrubber (no React, no fetch). A native <input
// type="date"> value is a plain YYYY-MM-DD with no timezone of its own,
// so a bound is always a LOCAL calendar day: `from` = that day's local
// midnight, `to` = its last second (23:59:59), both floored to Unix
// seconds — the shape `read_from`/`read_to` want on the wire (a window
// over `history`'s `started_at`, not the docs-list `mtime_unix` axis
// `from`/`to` already own under invariant #35 — same names, different
// table, kept as distinct URL params `read_from`/`read_to` so the two
// never collide).
//
// invariant:8 — read-only over the history table; nothing here writes.

import { dayBucket } from "../../lib/time";

function parseDateInput(value: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value.trim());
  if (!m) return null;
  const year = Number(m[1]);
  const month = Number(m[2]);
  const day = Number(m[3]);
  const d = new Date(year, month - 1, day);
  // Reject out-of-range components (e.g. 2026-02-30) that the Date
  // constructor would otherwise silently roll into the next month.
  if (
    d.getFullYear() !== year ||
    d.getMonth() !== month - 1 ||
    d.getDate() !== day
  ) {
    return null;
  }
  return d;
}

/// `<input type="date">` value -> local midnight, as Unix seconds.
/// `null` on an empty/malformed value (caller treats that as "unset").
export function dateInputToFromUnix(value: string): number | null {
  const d = parseDateInput(value);
  if (!d) return null;
  d.setHours(0, 0, 0, 0);
  return Math.floor(d.getTime() / 1000);
}

/// `<input type="date">` value -> the LAST second of that local day, as
/// Unix seconds (an inclusive upper bound).
export function dateInputToToUnix(value: string): number | null {
  const d = parseDateInput(value);
  if (!d) return null;
  d.setHours(23, 59, 59, 0);
  return Math.floor(d.getTime() / 1000);
}

/// The inverse of the two functions above: a Unix-seconds bound back to a
/// `YYYY-MM-DD` `<input type="date">` value (reuses `dayBucket`'s
/// local-calendar-day grammar, so both directions agree). `""` when
/// absent/non-finite.
export function unixToDateInput(unixSecs: number | null | undefined): string {
  if (unixSecs == null || !Number.isFinite(unixSecs)) return "";
  return dayBucket(unixSecs);
}

export type TemporalPreset = "today" | "7d" | "30d";

/// Preset "read during" windows -- inclusive `[from, to]` Unix-second
/// pairs ending at the end of today. `now` is injectable for
/// deterministic tests (mirrors `relativeAge`'s `nowMs` parameter).
export function presetRange(
  preset: TemporalPreset,
  now: Date = new Date(),
): { from: number; to: number } {
  const end = new Date(now);
  end.setHours(23, 59, 59, 0);
  const daysBack = preset === "today" ? 0 : preset === "7d" ? 6 : 29;
  const start = new Date(now);
  start.setDate(start.getDate() - daysBack);
  start.setHours(0, 0, 0, 0);
  return {
    from: Math.floor(start.getTime() / 1000),
    to: Math.floor(end.getTime() / 1000),
  };
}

/// Human label for the active-filter chip -- "read since D", "read until
/// D", "read D" (single day), or "read D - D" (a real span). `""` when
/// neither bound is set (caller omits the chip entirely).
export function formatReadWindow(
  from: number | null | undefined,
  to: number | null | undefined,
): string {
  const f = unixToDateInput(from);
  const t = unixToDateInput(to);
  if (f && t) return f === t ? `read ${f}` : `read ${f} – ${t}`;
  if (f) return `read since ${f}`;
  if (t) return `read until ${t}`;
  return "";
}
