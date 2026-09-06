import { useState } from "react";
import { Link } from "react-router-dom";
import { Icon } from "./icons";
import { useResurface } from "../hooks/useResurface";
import { artifactHref } from "../lib/artifactHref";
import { censusBump } from "../lib/census";
import { ResurfaceReview, resurfaceScoreTerms } from "./ResurfaceReview";
import { ScoreExplain, fmtScore } from "./ScoreExplain";

// Pull-only resurface strip — top 2 queue items on the unfiltered gallery.
// Anti-feature contract (docs/research/kb-resurface-queue-2026-07.html):
// renders NOTHING when the queue is empty (no celebration), carries no
// counts into persistent chrome beyond the "review N" button (the queue's
// own size — not a badge, just the one number the strip already implies),
// and the only dismiss is per-tab (sessionStorage) — the server never
// stores queue state.
const DISMISS_KEY = "kb:resurface:hidden";

export function ResurfaceStrip({ kb }: { kb: string | null }) {
  const [hidden, setHidden] = useState(
    () => sessionStorage.getItem(DISMISS_KEY) === "1",
  );
  const [reviewOpen, setReviewOpen] = useState(false);
  const q = useResurface(kb, !hidden);
  const all = q.data?.items ?? [];
  const weights = q.data?.weights;
  const items = all.slice(0, 2);
  if (hidden || !kb || items.length === 0) return null;
  return (
    <div
      className="gallery-resurface-strip"
      role="region"
      aria-label="Pick up where you left off"
    >
      <span className="gallery-resurface-strip__lead">◆ Pick up</span>
      {items.map((it) => (
        <div key={it.id} className="gallery-resurface-strip__row">
          <Link
            to={artifactHref(kb, it.source_relative)}
            className="gallery-resurface-strip__item"
            title={it.reasons.join(" · ")}
            onClick={() => censusBump("resurface.click")}
          >
            <span className="gallery-resurface-strip__title">{it.title}</span>
            <span className="gallery-resurface-strip__why">{it.reasons[0]}</span>
          </Link>
          <ScoreExplain
            label="resurface score"
            total={it.score}
            terms={resurfaceScoreTerms(it, weights)}
            footnote="score = comment + read"
            className="gallery-resurface-strip__chip"
          >
            {fmtScore(it.score)}
          </ScoreExplain>
        </div>
      ))}
      <div className="gallery-resurface-strip__controls">
        <button
          type="button"
          className="gallery-resurface-strip__review"
          data-kb-act="resurface-review"
          onClick={() => setReviewOpen(true)}
        >
          review {all.length}
        </button>
        <button
          type="button"
          className="gallery-resurface-strip__hide"
          aria-label="Hide until next session"
          title="Hide until next session"
          onClick={() => {
            sessionStorage.setItem(DISMISS_KEY, "1");
            setHidden(true);
          }}
        >
          <Icon.X />
        </button>
      </div>
      {reviewOpen && (
        <ResurfaceReview
          kb={kb}
          items={all}
          weights={weights}
          onClose={() => setReviewOpen(false)}
        />
      )}
    </div>
  );
}
