import { formatPane2, type PaneLoc } from "./paneUrl";

// SH.B3 — the inspector's Topics section (PreviewInspector.tsx) can be
// showing either the PRIMARY artifact or, in a two-pane split, pane 2's
// artifact: the rail is keyed to whichever pane is FOCUSED (invariant #30),
// and PreviewInspector has no `iframeRef` (it's a sibling of ArtifactPane,
// not a child), so a Topics row click can't post `kb:scroll-to-id` straight
// into the live frame the way TocSpy does. Instead it writes into the SAME
// URL param the focused pane's ArtifactPane already reacts to for every
// other same-artifact jump (permalinks, register marks, trail nav — RLs1):
// `?sec=` for the primary artifact, or the third field of `?pane2=` when
// the open doc IS pane 2's artifact (the `?pane2=` grammar, paneUrl.ts).
// Pure + total: never mutates its inputs, never throws.
export type TocJumpTarget =
  | { param: "sec"; value: string }
  | { param: "pane2"; value: string };

export function tocJumpParams(
  headingId: string,
  kb: string,
  sourceRelative: string,
  pane2: PaneLoc | null,
): TocJumpTarget {
  if (pane2 && pane2.kb === kb && pane2.sourceRelative === sourceRelative) {
    return { param: "pane2", value: formatPane2({ ...pane2, sec: headingId }) };
  }
  return { param: "sec", value: headingId };
}
