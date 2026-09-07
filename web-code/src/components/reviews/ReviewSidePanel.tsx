import { Link } from "react-router-dom";
import { useAgentReplyToast, useReviewComments } from "../../hooks/useReviewComments";
import { useReviewFindings } from "../../hooks/useReviews";
import { useSessionDiff } from "../../hooks/useSessionDiff";
import { Icon } from "../icons";
import AskAgentCard from "./AskAgentCard";
import ReviewThreadsCard from "./ReviewThreadsCard";
// ── V73-K2b (kbc-review/1) — the Document tab's ref jump list ──
import RefCardsCard from "./RefCardsCard";
import type { ReviewDocCard } from "../../api/types";
// ── PRR-U56 (§2 S2 side panel — Publish card + addendum-2 §F's Suggestions
// batch card) ──
import PublishCard from "./PublishCard";
import SuggestionsBatchCard from "./SuggestionsBatchCard";

export interface ReviewSidePanelProps {
  repo: string;
  id: number;
  ps: string;
  sessionId: string | null;
  onOpenFile: (path: string) => void;
  /// V4.M1 — phone-only: `role="dialog"` / `aria-modal` / labelled ✕.
  /// `false` on desktop so the in-grid `<aside>` stays byte-identical
  /// (root CLAUDE.md invariant #30, InspectorRail's `asSheet`).
  asSheet?: boolean;
  onMobileClose?: () => void;
  /// PRR-U56 — opens the `PublishPreview` modal (owned/rendered by
  /// `ReviewDetail.tsx`, which also owns the `?publish=1` URL sync).
  onOpenPublishPreview: () => void;
  /// PRR-F (design-addendum-2.md §A) — threaded to `ReviewThreadsCard`'s
  /// "GitHub (N)" filter chip; `undefined` for a non-PR-bound review.
  prNumber?: number;
  /// V73-K2b — the document's resolved ref cards, in wire order. `undefined`
  /// on every tab but Document, so the card simply is not there rather than
  /// being an empty list a reader would have to interpret. This panel does
  /// NOT fetch them: `ReviewDetail` owns the one `useReviewDoc` query and
  /// both the center and this rail render what it returned (the "one read,
  /// two renderers" rule the dossier already follows).
  docCards?: ReviewDocCard[];
  focusedRef?: string | null;
  onFocusRef?: (ref: string) => void;
}

export default function ReviewSidePanel({
  repo,
  id,
  ps,
  sessionId,
  onOpenFile,
  asSheet = false,
  onMobileClose,
  onOpenPublishPreview,
  prNumber,
  docCards,
  focusedRef = null,
  onFocusRef,
}: ReviewSidePanelProps) {
  const sessionDiff = useSessionDiff(sessionId ?? undefined, repo, !!sessionId);

  // PRR-U4 (§4) — the "✳ claude replied" toast. Reuses the SAME query keys
  // `ReviewThreadsCard` below already subscribes to (`useReviewComments`/
  // `useReviewFindings` dedupe by key — TanStack Query shares the cache
  // entry + network fetch across both mounts, no extra round-trip).
  const commentsQ = useReviewComments(repo, id, ps, true);
  const findingsQ = useReviewFindings(repo, id, { ps });
  useAgentReplyToast(repo, id, commentsQ.data, findingsQ.data?.findings ?? []);

  return (
    <aside
      className="kbc-review__side"
      data-region="review-side"
      {...(asSheet
        ? {
            id: "kbc-review-sheet",
            role: "dialog" as const,
            "aria-modal": true,
            "aria-label": "Review panel",
          }
        : {})}
    >
      {onMobileClose && (
        <header className="kbc-review__sheet-head">
          <span className="kbc-review__sheet-lab">Review panel</span>
          <button
            type="button"
            className="kbc-review__sheet-x"
            onClick={onMobileClose}
            aria-label="close review panel"
            data-kbc-review-sheet-close
          >
            <Icon.X />
          </button>
        </header>
      )}
      {docCards && docCards.length > 0 && onFocusRef && (
        <RefCardsCard cards={docCards} focusedRef={focusedRef} onFocus={onFocusRef} />
      )}

      <ReviewThreadsCard repo={repo} reviewId={id} ps={ps} onOpenFile={onOpenFile} prNumber={prNumber} />

      <AskAgentCard repo={repo} reviewId={id} />

      {/* ── PRR-U56 — Suggestions batch apply + Publish preview cards ── */}
      <SuggestionsBatchCard repo={repo} reviewId={id} ps={ps} />
      <PublishCard reviewId={id} onOpenPreview={onOpenPublishPreview} />

      <section className="kbc-review__card kbc-review__intent" data-kbc-review-intent>
        <h2 className="kbc-review__card-title">Intent</h2>
        {sessionId == null ? (
          <p className="kbc-review__card-empty" data-kbc-review-intent-empty>
            no session data
          </p>
        ) : sessionDiff.isError ? (
          <p className="kbc-review__card-empty" data-kbc-review-intent-loopback>
            session detail is loopback-only
          </p>
        ) : sessionDiff.isLoading ? (
          <p className="kbc-review__card-empty">Loading session…</p>
        ) : (
          <p className="kbc-review__card-empty">
            Session{" "}
            <Link to={`/session/${encodeURIComponent(sessionId)}/diff`} data-kbc-review-intent-link>
              {sessionId}
            </Link>
          </p>
        )}
      </section>
    </aside>
  );
}
