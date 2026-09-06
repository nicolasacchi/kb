import { Link } from "react-router-dom";
import type { ReviewReadingOrderOut } from "../../api/types";
import EmptyState from "../EmptyState";
import { Icon } from "../icons";
import { reviewDiffHref } from "./ReviewHeader";

export interface ReadingOrderPanelProps {
  repo: string;
  reviewId: number;
  loading: boolean;
  error: Error | null;
  data: ReviewReadingOrderOut | null | undefined;
  tourIdx: number;
  setTourIdx: (n: number | ((c: number) => number)) => void;
  onOpenFile: (path: string) => void;
}

export default function ReadingOrderPanel({
  repo,
  reviewId,
  loading,
  error,
  data,
  tourIdx,
  setTourIdx,
  onOpenFile,
}: ReadingOrderPanelProps) {
  if (loading) return <div className="kbc-reader__hint">Loading reading order…</div>;
  if (error) return <div className="kbc-reader__hint kbc-reader__hint--error">{error.message}</div>;
  if (data === null) {
    return (
      <div className="kbc-reader__hint" data-kbc-review-order-absent>
        Reading order not available on this server.
      </div>
    );
  }
  const stops = data?.stops ?? [];
  if (stops.length === 0) {
    // Same rule as the map: inputs_missing speaks even with zero stops.
    return (
      <div className="kbc-review__order" data-kbc-review-order>
        {data && data.inputs_missing.length > 0 && (
          <div className="kbc-recipes__banner" role="status" data-kbc-review-order-missing>
            not computed: {data.inputs_missing.join(", ")}
          </div>
        )}
        <EmptyState icon={<Icon.List />} title="No stops" hint="This patchset has no files to order." />
      </div>
    );
  }
  const idx = Math.max(0, Math.min(tourIdx, stops.length - 1));
  const current = stops[idx];

  return (
    <div className="kbc-review__order" data-kbc-review-order>
      {data && data.inputs_missing.length > 0 && (
        <div className="kbc-recipes__banner" role="status" data-kbc-review-order-missing>
          not computed: {data.inputs_missing.join(", ")}
        </div>
      )}
      <div className="kbc-review__tour" data-kbc-review-tour>
        <Link
          to={`${reviewDiffHref(repo, reviewId)}?file=${encodeURIComponent(stops[0].path)}`}
          className="kbc-review__action"
          onClick={() => setTourIdx(0)}
          data-kbc-review-tour-start
        >
          Start tour
        </Link>
        <button
          type="button"
          className="kbc-review__action"
          disabled={idx <= 0}
          onClick={() => {
            const next = Math.max(0, idx - 1);
            setTourIdx(next);
            onOpenFile(stops[next].path);
          }}
          data-kbc-review-tour-prev
        >
          Prev
        </button>
        <span className="kbc-review__tour-pos" data-kbc-review-tour-pos>
          {idx + 1} / {stops.length}
        </span>
        <button
          type="button"
          className="kbc-review__action"
          disabled={idx >= stops.length - 1}
          onClick={() => {
            const next = Math.min(stops.length - 1, idx + 1);
            setTourIdx(next);
            onOpenFile(stops[next].path);
          }}
          data-kbc-review-tour-next
        >
          Next
        </button>
        {current && (
          <span className="kbc-reviews__row-time" data-kbc-review-tour-current>
            {current.path}
          </span>
        )}
      </div>
      <ol className="kbc-review__stops" data-kbc-review-stops>
        {stops.map((s, i) => (
          <li
            key={`${s.path}-${i}`}
            className={
              "kbc-review__stop" + (i === idx ? " kbc-review__stop--active" : "")
            }
            data-kbc-review-stop={s.path}
            data-kbc-review-stop-cycle={s.cycle ? "1" : "0"}
          >
            <button
              type="button"
              className="kbc-review__stop-btn"
              onClick={() => {
                setTourIdx(i);
                onOpenFile(s.path);
              }}
            >
              <span className="kbc-review__stop-n">{i + 1}.</span>
              <span className="kbc-review__stop-path">{s.path}</span>
              <span className="kbc-review__stop-reason">{s.reason}</span>
              {s.cycle && (
                <span className="kbc-review__stop-cycle" data-kbc-review-cycle>
                  cycle
                </span>
              )}
            </button>
          </li>
        ))}
      </ol>
    </div>
  );
}
