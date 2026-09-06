import { useState } from "react";
import { ApiError } from "../../api/client";
import type { ReviewDetail, ReviewVerdict, ReviewVerdictState } from "../../api/types";
import { usePutReviewVerdict } from "../../hooks/useReviews";
import { Icon } from "../icons";
import { LOOPBACK_HINT, isLoopbackRefusal } from "./ReviewHeader";

const STATES: { key: ReviewVerdictState; label: string }[] = [
  { key: "comment", label: "Comment" },
  { key: "approve", label: "Approve" },
  { key: "request-changes", label: "Request changes" },
];

function stateLabel(state: ReviewVerdictState): string {
  if (state === "request-changes") return "changes requested";
  if (state === "approve") return "approved";
  return "commented";
}

function StateGlyph({ state }: { state: ReviewVerdictState }) {
  if (state === "approve") return <Icon.Check />;
  if (state === "request-changes") return <Icon.Warn />;
  return <Icon.Comment />;
}

export function VerdictChip({
  verdict,
  stale,
  latestPs,
}: {
  verdict: ReviewVerdict | null | undefined;
  stale?: boolean;
  latestPs?: number | null;
}) {
  if (!verdict) return null;
  const staleHint =
    stale && verdict.ps != null && latestPs != null && latestPs > verdict.ps
      ? `${stateLabel(verdict.state)} at ps${verdict.ps} — ps${latestPs} landed`
      : stale
        ? `${stateLabel(verdict.state)} at ps${verdict.ps ?? "?"} — newer patchset landed`
        : null;
  return (
    <span
      className={`kbc-verdict-chip kbc-verdict-chip--${verdict.state}`}
      data-kbc-review-verdict-chip={verdict.state}
      title={verdict.note ?? undefined}
    >
      <StateGlyph state={verdict.state} />
      {verdict.state}
      {staleHint && (
        <span className="kbc-verdict-chip__stale" data-kbc-review-verdict-stale title={staleHint}>
          stale
        </span>
      )}
    </span>
  );
}

export interface VerdictBarProps {
  repo: string;
  reviewId: number;
  review: ReviewDetail;
}

/// Segmented review-pass verdict. PUT is loopback-only — a 404 latches an
/// inline hint (StartReviewDialog precedent) so later clicks don't retry.
export default function VerdictBar({ repo, reviewId, review }: VerdictBarProps) {
  const put = usePutReviewVerdict(repo);
  const [noteOpen, setNoteOpen] = useState(false);
  const [note, setNote] = useState(review.verdict?.note ?? "");
  const [loopback, setLoopback] = useState(false);
  const latestPs = review.patchsets.length > 0 ? review.patchsets[review.patchsets.length - 1].ps_number : null;

  async function choose(state: ReviewVerdictState, withNote?: string) {
    if (loopback) return;
    try {
      const trimmed = (withNote ?? note).trim();
      await put.mutateAsync({
        id: reviewId,
        input: { state, note: trimmed || undefined },
      });
      setNoteOpen(false);
    } catch (e) {
      if (isLoopbackRefusal(e) || (e instanceof ApiError && e.status === 404)) {
        setLoopback(true);
      }
    }
  }

  return (
    <div className="kbc-verdict" data-kbc-review-verdict-bar>
      <div className="kbc-verdict__seg" role="group" aria-label="review verdict">
        {STATES.map((s) => (
          <button
            key={s.key}
            type="button"
            className={
              "kbc-verdict__btn" + (review.verdict?.state === s.key ? " is-active" : "")
            }
            aria-pressed={review.verdict?.state === s.key}
            disabled={loopback || put.isPending}
            onClick={() => void choose(s.key)}
            data-kbc-review-verdict={s.key}
          >
            <StateGlyph state={s.key} />
            {s.label}
          </button>
        ))}
      </div>
      <VerdictChip verdict={review.verdict} stale={review.verdict_stale} latestPs={latestPs} />
      {review.verdict_stale && review.verdict && (
        <span className="kbc-verdict__hint" data-kbc-review-verdict-stale-hint>
          {stateLabel(review.verdict.state)} at ps{review.verdict.ps ?? "?"}
          {latestPs != null ? ` — ps${latestPs} landed` : " — newer patchset landed"}
        </span>
      )}
      {loopback && (
        <p className="kbc-verdict__loopback" data-kbc-review-verdict-loopback>
          {LOOPBACK_HINT}
        </p>
      )}
      {review.verdict && !loopback && (
        <button
          type="button"
          className="kbc-verdict__note-toggle"
          onClick={() => {
            setNote(review.verdict?.note ?? "");
            setNoteOpen((v) => !v);
          }}
          data-kbc-review-verdict-note-toggle
        >
          Note
        </button>
      )}
      {noteOpen && review.verdict && !loopback && (
        <div className="kbc-verdict__popover" role="dialog" aria-label="verdict note">
          <input
            type="text"
            className="kbc-verdict__note"
            value={note}
            onChange={(e) => setNote(e.target.value)}
            placeholder="Optional note"
            aria-label="verdict note"
            data-kbc-review-verdict-note
            autoFocus
          />
          <div className="kbc-verdict__popover-actions">
            <button type="button" onClick={() => setNoteOpen(false)}>
              Cancel
            </button>
            <button
              type="button"
              onClick={() => void choose(review.verdict!.state, note)}
              disabled={put.isPending}
              data-kbc-review-verdict-confirm
            >
              {put.isPending ? "Saving…" : "Save note"}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
