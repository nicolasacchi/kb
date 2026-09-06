// W2.10 — pure grid math + density bucketing for the gallery's activity
// calendar. Deterministic, dependency-free (no Date-relative "now" —
// every input is an explicit [from, to] day range, so a golden test never
// drifts with the clock). Days are UTC calendar days end-to-end: the
// server aggregates `history` rows via sqlite's UTC-only `unixepoch`
// modifier (mirrors the `echoes` route's UTC convention), and this module
// treats every `YYYY-MM-DD` string as a UTC date — never local-parsed,
// always `Date.UTC(...)` built and `getUTC*` read — so the grid lines up
// with the server's day boundaries regardless of the browser's timezone.
//
// Calm-computing contract (the kb-resurface ruling, carried into every
// pull-only surface since): the grid is density only — no streak counts,
// no "longest run", no totals framed as an achievement. The one line of
// prose a caller renders is a plain count ("N events this year"), computed
// here as `totalEvents`.

export type CalendarDayCount = {
  day: string;
  opens: number;
  searches: number;
  comments: number;
};

export type DensityLevel = 0 | 1 | 2 | 3 | 4;

export type CalendarCell = {
  day: string;
  /** 0 (Sun) .. 6 (Sat), UTC. */
  weekday: number;
  /** 0-based grid column index. */
  week: number;
  total: number;
  opens: number;
  searches: number;
  comments: number;
  level: DensityLevel;
};

export type CalendarMonthLabel = {
  week: number;
  label: string;
};

export type CalendarGrid = {
  cells: CalendarCell[];
  /** Total column count — the grid's width. */
  weeks: number;
  monthLabels: CalendarMonthLabel[];
  totalEvents: number;
};

const MONTH_LABELS = [
  "Jan", "Feb", "Mar", "Apr", "May", "Jun",
  "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
] as const;

const DAY_MS = 24 * 60 * 60 * 1000;

/** Parse a `YYYY-MM-DD` string as a UTC-midnight timestamp (ms). No
 * timezone reinterpretation — `"2021-01-01"` is always UTC midnight,
 * regardless of the browser's local zone (unlike `new Date("2021-01-01")`,
 * which some engines parse as local midnight). */
function parseUtcDay(day: string): number {
  const [y, m, d] = day.split("-").map(Number);
  return Date.UTC(y, m - 1, d);
}

function formatUtcDay(ms: number): string {
  const d = new Date(ms);
  const y = d.getUTCFullYear();
  const m = String(d.getUTCMonth() + 1).padStart(2, "0");
  const dd = String(d.getUTCDate()).padStart(2, "0");
  return `${y}-${m}-${dd}`;
}

/** The UTC `YYYY-MM-DD` day label for a unix-seconds timestamp — the
 * public form of `formatUtcDay`, for callers (e.g. `ActivityCalendar`)
 * that compute a `[from, to]` request window in unix seconds and need the
 * matching day-string bounds for `buildCalendarGrid`. */
export function unixToUtcDay(unixSeconds: number): string {
  return formatUtcDay(unixSeconds * 1000);
}

/** Bucket a raw count into one of 5 density levels: 0 = no events, 1-4 =
 * quartile bands of `(0, max]`. Plain fraction thresholds — no log scale,
 * no outlier clamping. A single very-active day just makes every other
 * day's band relatively quieter, which is an honest density read (and
 * avoids a second seed-shaped knob to tune). */
export function densityLevel(count: number, max: number): DensityLevel {
  if (count <= 0 || max <= 0) return 0;
  const frac = count / max;
  if (frac > 0.75) return 4;
  if (frac > 0.5) return 3;
  if (frac > 0.25) return 2;
  return 1;
}

/** Build the full `[from, to]` (inclusive, UTC `YYYY-MM-DD`) day grid,
 * filling every day in range — days absent from `days` (the server only
 * returns days with at least one event) become zero-count cells, so the
 * grid always reads as a complete rectangle, never a sparse list.
 * Sunday-start columns (`week` = 0-based column index counted from the
 * grid's first Sunday on-or-before `from`), matching the familiar
 * contribution-graph shape. `from > to` yields an empty grid. */
export function buildCalendarGrid(
  from: string,
  to: string,
  days: CalendarDayCount[],
): CalendarGrid {
  const byDay = new Map(days.map((d) => [d.day, d]));
  const fromMs = parseUtcDay(from);
  const toMs = parseUtcDay(to);
  const gridStartMs = fromMs - new Date(fromMs).getUTCDay() * DAY_MS;

  const cells: CalendarCell[] = [];
  const monthLabels: CalendarMonthLabel[] = [];
  let totalEvents = 0;
  let lastLabeledMonth = -1;

  for (let ms = fromMs; ms <= toMs; ms += DAY_MS) {
    const day = formatUtcDay(ms);
    const weekday = new Date(ms).getUTCDay();
    const week = Math.floor((ms - gridStartMs) / DAY_MS / 7);
    const row = byDay.get(day);
    const opens = row?.opens ?? 0;
    const searches = row?.searches ?? 0;
    const comments = row?.comments ?? 0;
    const total = opens + searches + comments;
    totalEvents += total;
    cells.push({ day, weekday, week, total, opens, searches, comments, level: 0 });

    const month = new Date(ms).getUTCMonth();
    if (weekday === 0 && month !== lastLabeledMonth) {
      monthLabels.push({ week, label: MONTH_LABELS[month] });
      lastLabeledMonth = month;
    }
  }

  const max = cells.reduce((m, c) => Math.max(m, c.total), 0);
  for (const c of cells) c.level = densityLevel(c.total, max);

  const weeks = cells.length > 0 ? cells[cells.length - 1].week + 1 : 0;
  return { cells, weeks, monthLabels, totalEvents };
}

/** The `[from, to]` unix-second bounds (inclusive) for one UTC calendar
 * day — what a day-cell click hands to `galleryUrl(kb, {from, to})`
 * (invariant #35's mtime-window deep-link grammar). */
export function dayBoundsUnix(day: string): { from: number; to: number } {
  const startMs = parseUtcDay(day);
  return {
    from: Math.floor(startMs / 1000),
    to: Math.floor((startMs + DAY_MS - 1) / 1000),
  };
}

function count(n: number, singular: string, pluralForm: string): string {
  return `${n} ${n === 1 ? singular : pluralForm}`;
}

/** Hover-title text for one cell: the day (labeled UTC, since the grid's
 * day boundaries are UTC and may not match the viewer's local calendar
 * day) plus the per-kind breakdown. */
export function cellTooltip(cell: CalendarCell): string {
  return (
    `${cell.day} (UTC) — ${count(cell.opens, "open", "opens")}, ` +
    `${count(cell.searches, "search", "searches")}, ` +
    `${count(cell.comments, "comment", "comments")}`
  );
}
