import type { ReviewPatchset } from "../../api/types";
import { formatUnixSeconds, relativeTime, shortSha } from "../../lib/format";

export interface PatchsetStripProps {
  patchsets: ReviewPatchset[];
  compareMode: boolean;
  fromPs: number | null;
  toPs: number | null;
  activePsNum: number | null;
  interdiffFrom: number | undefined;
  interdiffTo: number | undefined;
  onSelectPs: (n: number) => void;
  onComparePick: (n: number) => void;
  onToggleCompare: () => void;
}

export default function PatchsetStrip({
  patchsets,
  compareMode,
  fromPs,
  toPs,
  activePsNum,
  interdiffFrom,
  interdiffTo,
  onSelectPs,
  onComparePick,
  onToggleCompare,
}: PatchsetStripProps) {
  return (
    <>
      <div className="kbc-review__ps-strip" data-kbc-review-ps-strip role="toolbar" aria-label="patchsets">
        {patchsets.map((p) => {
          const isActive = !compareMode && activePsNum === p.ps_number;
          const isFrom = compareMode && fromPs === p.ps_number;
          const isTo = compareMode && toPs === p.ps_number;
          return (
            <button
              key={p.ps_number}
              type="button"
              className={
                "kbc-review__ps-chip" +
                (isActive ? " kbc-review__ps-chip--active" : "") +
                (isFrom ? " kbc-review__ps-chip--from" : "") +
                (isTo ? " kbc-review__ps-chip--to" : "")
              }
              title={`ps${p.ps_number} · ${formatUnixSeconds(p.captured_at)} · ${shortSha(p.tip_sha_full || p.tip_sha)}`}
              onClick={() => (compareMode ? onComparePick(p.ps_number) : onSelectPs(p.ps_number))}
              data-kbc-review-ps={p.ps_number}
            >
              ps{p.ps_number}
              <span className="kbc-reviews__row-time"> · {relativeTime(p.captured_at)}</span>
            </button>
          );
        })}
        <button
          type="button"
          className={"kbc-review__compare-toggle" + (compareMode ? " kbc-review__compare-toggle--on" : "")}
          onClick={onToggleCompare}
          data-kbc-review-compare
        >
          {compareMode ? "Comparing…" : "Compare two"}
        </button>
      </div>
      {compareMode && (
        <p className="kbc-review__compare-hint" data-kbc-review-compare-hint>
          {fromPs == null
            ? "Pick the FROM patchset chip."
            : toPs == null
              ? `FROM ps${fromPs} — pick the TO patchset chip.`
              : `Interdiff ps${interdiffFrom} → ps${interdiffTo}.`}
        </p>
      )}
    </>
  );
}
