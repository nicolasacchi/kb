// PRR-U2 §2 S2 + §8 (states, "Head drift") — the amber staleness bar.
// Renders on the ONE staleness signal that exists today (`review.
// verdict_stale`, V4.C2); designed to ALSO carry a PR-head-drift message
// once a concurrent unit lands that signal (`GET /api/reviews/{id}/pr-status`,
// design-server.md §2 row 12 — Phase 4, not yet built) — the `prHeadDrift`
// prop is optional and simply unused today, tolerating its absence rather
// than requiring it.
import type { ReviewDetail } from "../../api/types";

export interface PrHeadDriftSignal {
  fromSha: string;
  toSha: string;
  /// Findings that lost their anchor after the head moved, if known.
  newlyOrphaned?: number;
}

/// Pure message-builder — a review-level verdict-staleness sentence
/// ("verdict set at psN — psM landed since") independent of the compact
/// inline hint `VerdictBar` already renders (that one lives INSIDE the
/// verdict card; this is the page-level banner per the mock's top bar).
/// Returns `null` when there is nothing stale to announce.
export function stalenessMessage(
  review: Pick<ReviewDetail, "verdict" | "verdict_stale">,
  latestPs: number | null,
): string | null {
  if (!review.verdict_stale || !review.verdict) return null;
  const verdictPs = review.verdict.ps;
  if (verdictPs != null && latestPs != null) {
    return `Your verdict was set at ps${verdictPs} — ps${latestPs} has landed since.`;
  }
  return "Your verdict is stale — a newer patchset has landed since it was set.";
}

export function prHeadDriftMessage(signal: PrHeadDriftSignal | null | undefined): string | null {
  if (!signal) return null;
  const short = (s: string) => s.slice(0, 12);
  const orphanNote =
    signal.newlyOrphaned != null
      ? signal.newlyOrphaned > 0
        ? ` ${signal.newlyOrphaned} finding${signal.newlyOrphaned === 1 ? "" : "s"} lost their anchor.`
        : " 0 orphaned so far."
      : "";
  return `PR head moved since this review — ${short(signal.fromSha)} → ${short(signal.toSha)}.${orphanNote}`;
}

export interface StalenessBannerProps {
  review: Pick<ReviewDetail, "verdict" | "verdict_stale">;
  latestPs: number | null;
  prHeadDrift?: PrHeadDriftSignal | null;
}

export default function StalenessBanner({ review, latestPs, prHeadDrift }: StalenessBannerProps) {
  const driftMsg = prHeadDriftMessage(prHeadDrift);
  const verdictMsg = stalenessMessage(review, latestPs);
  if (!driftMsg && !verdictMsg) return null;

  return (
    <div className="kbc-stale-banner" data-kbc-staleness-banner role="status">
      <span className="glyph" aria-hidden="true">
        ⚠
      </span>
      <span data-kbc-staleness-message>{driftMsg ?? verdictMsg}</span>
    </div>
  );
}
