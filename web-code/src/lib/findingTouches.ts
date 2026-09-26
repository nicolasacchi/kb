// V80-F3 — "lines changed in ps N". Pure helpers over a `ReviewFinding`'s
// `touched_in` field (`GET /api/reviews/{id}/findings`'s additive
// per-finding array): a caption chip for finding cards/rows
// (`touchedInChips`), and a Timeline-tab interleave
// (`findingTouchTimelineRows`), following the SAME "merge client-side, no
// second server kind" pattern `lib/githubThreads.ts`'s `githubTimelineRows`
// already establishes for GitHub rows.
//
// This is EVIDENCE, never a verdict: the daemon computed `touched_in` per
// read and never persisted it (`review_finding_touches.rs`'s own doc), and
// this module adds no interpretation on top — it only formats. The word
// "fixed" must never appear in any label/title this file produces.
import type { FindingTouchedIn, ReviewFinding, ReviewPatchset } from "../api/types";
import { reviewDiffHref } from "./codeUrl";
import type { TimelineRow } from "./reviewTimeline";

export interface TouchedInChip {
  ps: number;
  label: string;
  title: string;
  href: string;
}

function overlapWord(overlap: FindingTouchedIn["overlap"]): string {
  return overlap === "exact" ? "exact" : "adjacent";
}

function hunkWord(hunks: number): string {
  return `${hunks} hunk${hunks === 1 ? "" : "s"}`;
}

/// One chip per `touched_in` entry — "lines changed in ps N" — deep-linking
/// to the INTERDIFF between the finding's own ps and that later ps, on the
/// finding's own file (`reviewDiffHref(..., { ps: {from, to}, file })`, the
/// SAME builder the review-diff patchset switcher uses). `[]` when the
/// finding carries no `own_ps` (an older daemon, or a should-never-happen
/// missing annotation) — a chip that cannot link anywhere honestly is no
/// chip at all.
export function touchedInChips(
  finding: Pick<ReviewFinding, "own_ps" | "touched_in" | "location">,
  repo: string,
  reviewId: number,
): TouchedInChip[] {
  const ownPs = finding.own_ps;
  if (ownPs == null) return [];
  return (finding.touched_in ?? []).map((t) => ({
    ps: t.ps,
    label: `lines changed in ps ${t.ps}`,
    title: `${overlapWord(t.overlap)} overlap — ${hunkWord(t.hunks)} in ps ${t.ps}'s diff from ps ${ownPs}`,
    href: reviewDiffHref(repo, reviewId, undefined, {
      ps: { from: ownPs, to: t.ps },
      file: finding.location.path,
    }),
  }));
}

/// `patchsets` (`GET /api/reviews/{id}`'s own `captured_at` per ps) resolve
/// each row's Timeline `at` — an honest `0` (floats to the oldest end,
/// never thrown) when a ps is missing from the list this call was given.
export function findingTouchTimelineRows(
  findings: readonly ReviewFinding[],
  patchsets: readonly ReviewPatchset[],
  repo: string,
  reviewId: number,
): TimelineRow[] {
  const capturedAt = new Map(patchsets.map((p) => [p.ps_number, p.captured_at]));
  const rows: TimelineRow[] = [];
  for (const finding of findings) {
    for (const chip of touchedInChips(finding, repo, reviewId)) {
      rows.push({
        at: capturedAt.get(chip.ps) ?? 0,
        kind: "finding_touch",
        icon: "finding_touch",
        label: `author touched ${finding.slug}'s lines in ps ${chip.ps}`,
        detail: chip.title,
        href: chip.href,
      });
    }
  }
  return rows;
}
