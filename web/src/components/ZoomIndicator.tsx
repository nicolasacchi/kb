import { ladderIndex, ZOOM_LABEL, ZOOM_LADDER } from "../lib/semanticZoom";

// Three ticks for Atlas / Index / Shelf, the current one filled. Hidden when
// the active Library view is not on the semantic-zoom ladder.
export default function ZoomIndicator({ view }: { view: string }) {
  const at = ladderIndex(view);
  if (at < 0) return null;
  return (
    <span
      className="kb-zoomind"
      role="img"
      aria-label={`Library zoom: ${ZOOM_LABEL[ZOOM_LADDER[at]]}`}
      title="Pinch, or press - and =, to zoom the Library: Atlas, Index, Shelf"
      data-testid="zoom-indicator"
    >
      {ZOOM_LADDER.map((v, i) => (
        <span key={v} className={i === at ? "kb-zoomind__tick is-on" : "kb-zoomind__tick"} data-zoom-tick={v} />
      ))}
    </span>
  );
}
