// The LINKED-TAB chip for a SERVER sequence (V74-L3b, D12 + §P7).
//
// V70-A6 shipped the browser-local half of this: a tab the Ramp opened wears
// `TrailOriginChip`, which says where the hop came from and walks back. This
// is the same idea for the two SERVER sequences a step can come from — a
// `kbc-tour/1` tour and a `kbc-trail/1` trail — and it deliberately rides the
// SAME `?trail=&step=` grammar with a `src=` discriminator rather than a
// second pair of params (`lib/codeUrl.ts`'s `TrailLinkSource`). One grammar,
// one parser, one strip list.
//
// What the chip promises, and the two rules behind it:
//
//   * **Following it re-focuses the STEP** — not "goes near it". A tour chip
//     navigates to that tour at that step (`?step=` is 1-based on the wire and
//     the conversion lives in `lib/toursUrl.ts`); a trail chip asks the rail
//     to open on that step. Either way the operator lands on the stop they
//     came from, which is the whole point of carrying the link.
//   * **No chip rather than a dead chip** — `TrailOriginChip`'s rule 2,
//     unchanged. A tour whose slug no longer resolves, or a trail this
//     browser cannot read (the human reads are loopback-only), renders
//     NOTHING. A chip that cannot go back is worse than no chip.

import { useNavigate } from "react-router";
import type { TrailLink } from "../../lib/codeUrl";
import { Icon } from "../icons";
import { useTour } from "../../hooks/useTours";
import { useTrail } from "../../hooks/useTrails";
import { tourHref } from "../../lib/toursUrl";
import { trailStepLabel } from "./TrailRail";

/// The event the trail chip fires. The rail lives inside the reader and the
/// chip lives in the app chrome, so "open the rail on this step" crosses a
/// component boundary the app has no shared store for — the same one-line
/// `CustomEvent` channel the omnibox opener uses (`kbc:omnibox.open`).
export const TRAIL_FOCUS_EVENT = "kbc:trail.focus";

export interface TrailFocusDetail {
  id: string;
  ordinal: number;
}

export function requestTrailFocus(detail: TrailFocusDetail): void {
  window.dispatchEvent(new CustomEvent<TrailFocusDetail>(TRAIL_FOCUS_EVENT, { detail }));
}

function TourChip({ repo, link }: { repo: string; link: TrailLink }) {
  const navigate = useNavigate();
  const tour = useTour(repo, link.id);
  const steps = tour.data?.steps ?? [];
  const step = steps[link.step];
  if (!tour.data || !step) return null;
  return (
    <span className="kbc-trailchip" data-kbc-linked-chip="tour" data-kbc-linked-id={link.id}>
      <button
        type="button"
        className="kbc-trailchip__back"
        data-kbc-linked-return
        title={`back to the tour "${tour.data.title}", step ${link.step + 1}`}
        onClick={() => navigate(tourHref(repo, link.id, { step: link.step }))}
      >
        <Icon.ArrowLeft width={12} height={12} aria-hidden />
        <span className="kbc-trailchip__text">
          from the tour "{tour.data.title}" &middot; step {link.step + 1} of {steps.length}
        </span>
      </button>
    </span>
  );
}

function TrailChip({ repo, link }: { repo: string; link: TrailLink }) {
  const trail = useTrail(repo, link.id);
  const steps = trail.data?.steps ?? [];
  const step = steps.find((s) => s.ordinal === link.step);
  if (!trail.data || !step) return null;
  return (
    <span className="kbc-trailchip" data-kbc-linked-chip="trail" data-kbc-linked-id={link.id}>
      <button
        type="button"
        className="kbc-trailchip__back"
        data-kbc-linked-return
        title="back to this step in your trail"
        onClick={() => requestTrailFocus({ id: link.id, ordinal: link.step })}
      >
        <Icon.ArrowLeft width={12} height={12} aria-hidden />
        <span className="kbc-trailchip__text">
          from your trail &middot; step {link.step} ({trailStepLabel(step)})
        </span>
      </button>
    </span>
  );
}

export interface LinkedStepChipProps {
  repo: string | null;
  link: TrailLink | null;
}

/// Renders the chip for a link that names a SERVER source, and nothing at all
/// otherwise — a `src`-less link is the browser-local trail, which
/// `TrailOriginChip` already owns.
export default function LinkedStepChip({ repo, link }: LinkedStepChipProps) {
  if (!repo || !link || !link.src) return null;
  if (link.src === "tour") return <TourChip repo={repo} link={link} />;
  if (link.src === "trail") return <TrailChip repo={repo} link={link} />;
  return null;
}
