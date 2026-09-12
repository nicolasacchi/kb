// PRR-U1 — kb v0.39 "The PR Room," unit U1 (design-ui.md §2 S1): the
// landing page's secondary "unreviewed PRs" strip — `GET /api/prs` rows
// minus every PR number a review already binds (`lib/reviewInbox.ts`'s
// `unreviewedPrs`). Deliberately NOT part of the inbox: design doc §2 S1
// says so verbatim ("PRs with no local review are NOT in the inbox
// route"), and the inbox route itself is local-only by construction
// (`review_inbox.rs`'s module doc) — this strip is the ONE place on the
// landing that talks to the GitHub read overlay, so it (and only it)
// degrades on `unavailable_reason` rather than blocking the room.
import { useState } from "react";
import { useNavigate } from "react-router";
import type { PrOut } from "../../api/types";
import { ApiError } from "../../api/client";
import { useCreateReviewPr } from "../../hooks/useReviews";
import { buildCreateReviewPrInput } from "../../lib/reviewInbox";
import { reviewUrl } from "../../lib/codeUrl";
import { relativeTime } from "../../lib/format";
import { toast } from "../../lib/toast";

export interface UnreviewedPrsStripProps {
  repo: string;
  prs: PrOut[];
  /// `PrsResponse.unavailable_reason` — present only when the GitHub call
  /// itself failed; `prs` is then always empty (see `fetchPrs`'s own doc).
  unavailableReason?: string;
}

/// Same LOOPBACK-ONLY latch shape `routes/Prs.tsx`'s own "Start review"
/// action uses (`ReviewHeader.tsx`'s `isLoopbackRefusal`/`LOOPBACK_HINT`
/// precedent) — kept local rather than imported, since a 404 latch is
/// per-component UI state, not shared logic.
export default function UnreviewedPrsStrip({ repo, prs, unavailableReason }: UnreviewedPrsStripProps) {
  const navigate = useNavigate();
  const createReviewPr = useCreateReviewPr(repo);
  const [startingNumber, setStartingNumber] = useState<number | null>(null);
  const [loopback, setLoopback] = useState(false);

  async function handleStartReview(pr: PrOut) {
    if (loopback) return;
    setStartingNumber(pr.number);
    try {
      const out = await createReviewPr.mutateAsync(buildCreateReviewPrInput(repo, pr));
      navigate(reviewUrl(repo, out.id));
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        setLoopback(true);
        toast.err("Starting a review is loopback-only — open kb-code from the machine running kb-code-server.");
      } else {
        const message = e instanceof Error ? e.message : String(e);
        toast.err(`couldn't start review: ${message}`);
      }
    } finally {
      setStartingNumber(null);
    }
  }

  if (!unavailableReason && prs.length === 0) return null;

  return (
    <section className="kbc-unreviewed" data-kbc-unreviewed>
      <h2 className="kbc-unreviewed__title">Unreviewed PRs</h2>
      {unavailableReason && (
        <p className="kbc-unreviewed__caption" data-kbc-unreviewed-unavailable>
          {unavailableReason}
        </p>
      )}
      {prs.length > 0 && (
        <ul className="kbc-unreviewed__list" data-kbc-unreviewed-list>
          {prs.map((pr) => (
            <li key={pr.number} className="kbc-unreviewed__row" data-kbc-unreviewed-row={pr.number}>
              <span className="kbc-unreviewed__number">#{pr.number}</span>
              <span className="kbc-unreviewed__title-text">{pr.title}</span>
              <span title={new Date(pr.updated_at).toLocaleString()}>
                {relativeTime(Date.parse(pr.updated_at) / 1000)}
              </span>
              <button
                type="button"
                className="kbc-unreviewed__cta"
                disabled={startingNumber === pr.number || loopback}
                title={loopback ? "loopback-only — open kb-code from the machine running kb-code-server" : undefined}
                onClick={() => void handleStartReview(pr)}
                data-kbc-unreviewed-start={pr.number}
              >
                {startingNumber === pr.number ? "Starting…" : "Start review"}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
