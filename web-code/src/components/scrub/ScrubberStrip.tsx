import {
  labelFor,
  shortSha,
  type ScrubFloor,
  type ScrubPos,
  type ScrubStop,
} from "../../lib/scrub";

const TICK_CAP = 48;

export interface ScrubberStripProps {
  stops: readonly ScrubStop[];
  floor: ScrubFloor | null | undefined;
  pos: ScrubPos;
  truncated?: boolean;
  total?: number;
  onStep: (pos: ScrubPos) => void;
  onWorkingTree: () => void;
  onClose: () => void;
}

/// File-scoped time scrubber. Lives inside `<main>` as a non-landmark
/// strip (the landmark golden is untouched). Esc is not bound — D2;
/// "working tree" is the explicit back action.
export default function ScrubberStrip({
  stops,
  floor,
  pos,
  truncated,
  total,
  onStep,
  onWorkingTree,
  onClose,
}: ScrubberStripProps) {
  const label = labelFor(pos, stops, floor);
  const currentIndex = pos.kind === "stop" ? pos.index : pos.kind === "working-tree" ? -1 : stops.length;
  const sliderMax = Math.max(stops.length, 1);

  function goIndex(i: number) {
    if (i < 0) {
      onWorkingTree();
      return;
    }
    if (i >= stops.length) {
      onStep({ kind: "before-floor" });
      return;
    }
    onStep({ kind: "stop", index: i, resolution: "nearest-prior" });
  }

  return (
    <div className="kbc-scrub" data-kbc-scrub-strip role="group" aria-label="file time scrubber">
      <span className="kbc-scrub__label" data-kbc-scrub-label>
        {label}
      </span>
      {stops.length <= TICK_CAP ? (
        <div className="kbc-scrub__ticks" role="group" aria-label="stops">
          {stops.map((s, i) => (
            <button
              key={s.sha}
              type="button"
              className={"kbc-scrub__tick" + (i === currentIndex ? " is-current" : "")}
              title={`${shortSha(s.sha)} · ${s.subject}`}
              aria-label={`stop ${shortSha(s.sha)}`}
              aria-current={i === currentIndex ? "true" : undefined}
              data-kbc-scrub-tick={s.sha}
              onClick={() => goIndex(i)}
            />
          ))}
        </div>
      ) : (
        <input
          type="range"
          className="kbc-scrub__slider"
          min={0}
          max={sliderMax}
          value={Math.min(Math.max(currentIndex, 0), sliderMax)}
          aria-label="scrub to a stop"
          data-kbc-scrub-slider
          onChange={(e) => goIndex(Number(e.target.value))}
        />
      )}
      <button
        type="button"
        className="kbc-scrub__wt"
        data-kbc-scrub-working-tree
        onClick={onWorkingTree}
      >
        working tree
      </button>
      {truncated ? (
        <span className="kbc-scrub__trunc" data-kbc-scrub-truncated>
          {stops.length} of {total ?? stops.length}
        </span>
      ) : null}
      <button type="button" className="kbc-scrub__close" data-kbc-scrub-close onClick={onClose} aria-label="close scrubber">
        ×
      </button>
    </div>
  );
}
