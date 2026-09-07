// `review-timeline/2` (V73-K2c) — the SPA's small, hand-mirrored copy of
// `review_timeline.rs`'s `LANES` constant (eleven lanes, composition
// order). Lane VISIBILITY is a pure client-side filter over the already-
// fetched `events[]` (the wire has no `?lane=` — narrowing round-trips only
// via `kind`/`author`/`since`/`until`), so this module owns exactly that:
// the lane list, its display label, and the pure toggle/cycle step
// `TimelinePanel.tsx` renders and the `review.timeline.lane-cycle`
// registry row drives.
export const TIMELINE_LANES = [
  "lifecycle",
  "pr_body",
  "findings",
  "verdict",
  "comments",
  "wt_comments",
  "document",
  "report",
  "claims",
  "github",
  "turns",
] as const;
export type TimelineLane = (typeof TIMELINE_LANES)[number];

export const TIMELINE_LANE_LABELS: Record<TimelineLane, string> = {
  lifecycle: "Lifecycle",
  pr_body: "PR description",
  findings: "Findings",
  verdict: "Verdict",
  comments: "Comments",
  wt_comments: "Working-tree comments",
  document: "Document",
  report: "Agent report",
  claims: "Claims",
  github: "GitHub",
  turns: "Turns",
};

export function laneLabel(lane: string): string {
  return TIMELINE_LANE_LABELS[lane as TimelineLane] ?? lane;
}

/// Toggle exactly one lane's membership in a hidden-set — pure, so
/// `TimelinePanel.tsx`'s click handler and its vitest coverage share one
/// implementation.
export function toggleLane(hidden: ReadonlySet<string>, lane: string): Set<string> {
  const next = new Set(hidden);
  if (next.has(lane)) next.delete(lane);
  else next.add(lane);
  return next;
}

/// `review.timeline.lane-cycle`'s step: advance the cursor and toggle the
/// lane it now points at. Wraps at the end of `TIMELINE_LANES`, so
/// repeated presses walk every lane exactly once before repeating.
export function cycleLaneStep(
  hidden: ReadonlySet<string>,
  cursor: number,
): { hidden: Set<string>; cursor: number; lane: TimelineLane } {
  const lane = TIMELINE_LANES[((cursor % TIMELINE_LANES.length) + TIMELINE_LANES.length) % TIMELINE_LANES.length];
  return { hidden: toggleLane(hidden, lane), cursor: (cursor + 1) % TIMELINE_LANES.length, lane };
}
