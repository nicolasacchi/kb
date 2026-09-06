// W3.C-b — the pure half of the multi-facet reflection canvas: four
// synchronized day tracks (creation · reading · sessions · comments) sharing
// ONE brush.
//
// Everything here is a total function of its arguments: no clock (never
// `Date.now()`), no DOM, no fetch, no React — so the whole grammar is
// golden-pinnable in `timeline.test.ts` and `ReflectionCanvas.tsx` is only
// wiring. The window is always handed in explicitly; the component pins it
// at mount (see ActivityCalendar.tsx's identical rule).
//
// UTC, in lock-step with `lib/calendar.ts`: the day math here does NOT fork
// the calendar's — `unixToUtcDay` / `dayBoundsUnix` / `densityLevel` are
// imported, not reimplemented. Every day-grained contract in this codebase
// (the server's `timeline`/`history/calendar` routes, the calendar grid, the
// `?from`/`?to` gallery window) is a UTC calendar day, and they must agree.
//
// THE HONESTY RULES (product, not style):
//  1. Per-track INDEPENDENT density maxima. A track with 3 comments must not
//     read as empty beside a track with 300 reads — each lane's ramp is
//     relative to its own busiest day, and the UI says so.
//  2. Density and evidence only. No streaks, no "best day", no goals, no
//     completion — the kb-resurface calm-computing ruling, same posture as
//     the activity calendar this sits beside.
//  3. Never silently truncate a pivot. The server's per-lane `truncated`
//     flag and the gallery's 500-id `?ids=` cap are surfaced as a plain
//     explanatory sentence (`PivotPlan`), never as a quietly shortened set.

import { dayBoundsUnix, densityLevel, unixToUtcDay, type DensityLevel } from "./calendar";
import { galleryUrl } from "./galleryUrl";
import type { TimelineResponse } from "../api/generated/TimelineResponse";
import type { TimelineTrack } from "../api/generated/TimelineTrack";

const DAY_SECS = 86400;

/// Hard stop on the day axis. The server caps a requested span at ~400 days
/// (`timeline::MAX_SPAN_SECS`); this is that plus slack, and exists purely so
/// a nonsense `[from, to]` can never spin the fill loop.
export const MAX_AXIS_DAYS = 512;

/// The lane order the canvas renders in — creation first (the corpus grows),
/// then the three ways a corpus is *used*. Also the union order `planPivot`
/// walks, so a pivot's id list is deterministic.
export const TRACK_ORDER: readonly TimelineTrack[] = [
  "created",
  "read",
  "session",
  "comment",
];

/// Short lane headings. The long, honest sentence under each one is the
/// SERVER's `lane.label` (it owns what the numbers mean); these are just the
/// column titles.
export const TRACK_TITLES: Record<TimelineTrack, string> = {
  created: "created",
  read: "read",
  session: "sessions",
  comment: "comments",
};

/// Used only when a response is missing a lane entirely (an older daemon, a
/// partial fetch). Says so rather than rendering a confident empty track.
const ABSENT_LABEL = "not reported by this daemon";

export type CanvasCell = {
  day: string;
  count: number;
  /// Density band relative to THIS lane's own busiest day (honesty rule 1).
  level: DensityLevel;
};

export type CanvasLane = {
  track: TimelineTrack;
  title: string;
  /// The server's own sentence about what this lane counts.
  label: string;
  /// One cell per day in the window, zero-filled, same length/order as every
  /// other lane's (the "synchronized" contract).
  cells: CanvasCell[];
  /// Sum over `cells` — events in the window, NOT distinct artifacts.
  total: number;
  /// This lane's busiest single day (the density denominator).
  max: number;
  /// Resolved artifact ids for the WHOLE window (not per day — the wire
  /// resolves the window, so a sub-window pivot must re-ask; see
  /// `planPivot`'s doc comment).
  ids: string[];
  truncated: boolean;
  /// False when the response carried no lane for this track.
  present: boolean;
};

export type CanvasLanes = {
  /// The shared x-axis: every UTC day in the window, ascending, zero-filled.
  days: string[];
  lanes: CanvasLane[];
};

/// Every UTC `YYYY-MM-DD` day in `[fromUnix, toUnix]`, ascending and
/// inclusive. Built by stepping whole UTC days off `dayBoundsUnix` (the
/// shipped calendar math) rather than by re-deriving date arithmetic.
/// `toUnix < fromUnix` yields an empty axis.
export function dayAxis(fromUnix: number, toUnix: number): string[] {
  if (!Number.isFinite(fromUnix) || !Number.isFinite(toUnix)) return [];
  if (toUnix < fromUnix) return [];
  const endDay = unixToUtcDay(toUnix);
  const out: string[] = [];
  let cursor = dayBoundsUnix(unixToUtcDay(fromUnix)).from;
  for (let i = 0; i < MAX_AXIS_DAYS; i += 1) {
    const day = unixToUtcDay(cursor);
    out.push(day);
    if (day >= endDay) break;
    cursor += DAY_SECS;
  }
  return out;
}

/// Build the four synchronized lanes for `[fromUnix, toUnix]`.
///
/// EVERY day in the window gets a cell in EVERY lane (the server already
/// zero-fills; this re-fills defensively so a short/sparse response can't
/// desynchronize the tracks), and each lane's density ramp is computed
/// against its own maximum — honesty rule 1.
///
/// A null/absent response yields four empty-but-present-shaped lanes over the
/// same axis, so the canvas renders its frame while loading instead of
/// jumping.
export function buildLanes(
  res: TimelineResponse | null | undefined,
  fromUnix: number,
  toUnix: number,
): CanvasLanes {
  const days = dayAxis(fromUnix, toUnix);
  const byTrack = new Map((res?.lanes ?? []).map((l) => [l.track, l]));
  const lanes = TRACK_ORDER.map((track): CanvasLane => {
    const wire = byTrack.get(track);
    const counts = new Map((wire?.days ?? []).map((d) => [d.day, d.count]));
    const raw = days.map((day) => Math.max(0, counts.get(day) ?? 0));
    const max = raw.reduce((m, c) => Math.max(m, c), 0);
    return {
      track,
      title: TRACK_TITLES[track],
      label: wire?.label ?? ABSENT_LABEL,
      cells: days.map((day, i) => ({
        day,
        count: raw[i],
        level: densityLevel(raw[i], max),
      })),
      total: raw.reduce((a, b) => a + b, 0),
      max,
      ids: wire?.ids ?? [],
      truncated: wire?.truncated ?? false,
      present: wire != null,
    };
  });
  return { days, lanes };
}

export type Brush = {
  /// Clamped, normalised indices into the day axis (inclusive both ends).
  fromIndex: number;
  toIndex: number;
  fromDay: string;
  toDay: string;
  /// Inclusive unix-second bounds: UTC midnight of `fromDay` through the last
  /// second of `toDay` (`dayBoundsUnix`, so they line up exactly with the
  /// gallery's `?from`/`?to` mtime window and with a re-request of this same
  /// route).
  fromUnix: number;
  toUnix: number;
  /// Every day the brush covers, ascending.
  days: string[];
};

/// Normalise a two-ended selection into a brush: a reversed drag (`a > b`)
/// swaps, out-of-range indices clamp to the window, non-finite input reads as
/// the corresponding window edge. Returns null only for an empty axis.
export function brushFromIndices(
  a: number,
  b: number,
  days: readonly string[],
): Brush | null {
  if (days.length === 0) return null;
  const last = days.length - 1;
  const clamp = (n: number, fallback: number): number => {
    if (!Number.isFinite(n)) return fallback;
    return Math.min(last, Math.max(0, Math.trunc(n)));
  };
  const ca = clamp(a, 0);
  const cb = clamp(b, last);
  const fromIndex = Math.min(ca, cb);
  const toIndex = Math.max(ca, cb);
  const fromDay = days[fromIndex];
  const toDay = days[toIndex];
  return {
    fromIndex,
    toIndex,
    fromDay,
    toDay,
    fromUnix: dayBoundsUnix(fromDay).from,
    toUnix: dayBoundsUnix(toDay).to,
    days: days.slice(fromIndex, toIndex + 1),
  };
}

/// Index of a `YYYY-MM-DD` day on the axis, or null when it isn't in the
/// window — what the accessible `<input type="date">` fallback resolves
/// through (so a typed date outside the pinned window is refused, not
/// silently clamped to an edge the operator didn't pick).
export function indexOfDay(day: string, days: readonly string[]): number | null {
  const i = days.indexOf(day);
  return i < 0 ? null : i;
}

/// Events (not distinct artifacts) a lane recorded inside the brush. Exact —
/// it sums the per-day counts, which are never capped.
export function laneEventsInBrush(lane: CanvasLane, brush: Brush): number {
  let sum = 0;
  for (let i = brush.fromIndex; i <= brush.toIndex && i < lane.cells.length; i += 1) {
    sum += lane.cells[i].count;
  }
  return sum;
}

/// The gallery's `?ids=` cap (`routes::docs::MAX_IDS_FILTER`). Over it the
/// server 400s rather than truncating, so the canvas must degrade BEFORE the
/// URL is built — never hand it a set it will reject.
export const PIVOT_ID_CAP = 500;

export type PivotPlan =
  /// Exact: open these artifacts as `galleryUrl(kb, { ids })`.
  | { kind: "ids"; ids: string[]; tracks: TimelineTrack[] }
  /// Creation-only fallback: the id set is over cap or truncated, but the
  /// `created` lane's axis IS the gallery's mtime window, so `?from`/`?to`
  /// answers the same question exactly (invariant #35's shipped atoms).
  | { kind: "window"; fromUnix: number; toUnix: number; reason: string }
  /// No honest pivot exists — the UI disables the control and prints this.
  | { kind: "blocked"; reason: string };

/// Decide what the brush can honestly open in the gallery.
///
/// `lanes` MUST be the lanes for the BRUSHED window: the wire resolves ids
/// per REQUEST window, not per day, so a sub-window pivot re-asks the same
/// route with the brush's bounds (a cache hit when the brush is the whole
/// pinned window — identical query key). Dragging never triggers that fetch;
/// only activating the pivot does.
///
/// The rules, in order:
///  - nothing selected → blocked;
///  - any selected lane `truncated`, or the union over cap → creation-only
///    brushes fall back to the mtime window (exact), mixed brushes are
///    BLOCKED with a sentence naming why (a partial id set would silently
///    lie about what the brush contains);
///  - an empty union → blocked ("nothing recorded"), so the operator never
///    lands on a mysteriously empty gallery.
export function planPivot(
  lanes: CanvasLanes,
  selected: readonly TimelineTrack[],
  brush: Brush,
): PivotPlan {
  const tracks = TRACK_ORDER.filter((t) => selected.includes(t));
  if (tracks.length === 0) {
    return { kind: "blocked", reason: "select at least one track to open." };
  }
  const createdOnly = tracks.length === 1 && tracks[0] === "created";
  const chosen = lanes.lanes.filter((l) => tracks.includes(l.track));
  const truncated = chosen.filter((l) => l.truncated).map((l) => l.title);

  const seen = new Set<string>();
  const ids: string[] = [];
  for (const lane of chosen) {
    for (const id of lane.ids) {
      if (!seen.has(id)) {
        seen.add(id);
        ids.push(id);
      }
    }
  }

  const overCap = ids.length > PIVOT_ID_CAP;
  if (truncated.length > 0 || overCap) {
    const why =
      truncated.length > 0
        ? `the daemon capped the ${truncated.join(" + ")} id set for this window`
        : `${ids.length} artifacts exceeds the ${PIVOT_ID_CAP}-id gallery cap`;
    if (createdOnly) {
      return {
        kind: "window",
        fromUnix: brush.fromUnix,
        toUnix: brush.toUnix,
        reason: `${why} — opening the date window instead, which is exact for creation.`,
      };
    }
    return {
      kind: "blocked",
      reason: `${why}. Narrow the brush, or select only "created" (its date window is exact).`,
    };
  }
  if (ids.length === 0) {
    return {
      kind: "blocked",
      reason: "nothing recorded on the selected tracks in this range.",
    };
  }
  return { kind: "ids", ids, tracks };
}

/// The one place a plan becomes a URL — always through `galleryUrl`
/// (invariant #35: reader/canvas → gallery deep-links have exactly ONE
/// builder, and this surface adds ZERO new filter atoms: it reuses the
/// shipped `ids` and `from`/`to` atoms only).
export function pivotUrl(kb: string, plan: PivotPlan): string | null {
  if (plan.kind === "ids") return galleryUrl(kb, { ids: plan.ids });
  if (plan.kind === "window") {
    return galleryUrl(kb, { from: plan.fromUnix, to: plan.toUnix });
  }
  return null;
}

/// One plain sentence describing the brush — "14 days · 2026-07-01 →
/// 2026-07-14 (UTC)". Density and evidence only; no achievement framing.
export function brushSummary(brush: Brush): string {
  const n = brush.days.length;
  return `${n} day${n === 1 ? "" : "s"} · ${brush.fromDay} → ${brush.toDay} (UTC)`;
}
