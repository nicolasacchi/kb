// PRR-F — the dialectic ledger (design-ui.md §12.1, frontier 1). Pure
// composition over the verdict strip's own two voices + the review's
// findings/comments — zero new storage (`lib/dialectic.ts`'s own doc).
// Renders nothing when the two verdicts don't actually disagree (§2 S2:
// "No report → strip collapses to the plain VerdictBar exactly as today" —
// this unit's own extension of that same "quiet by default" law).
import { useMemo, useState } from "react";
import type { ReviewDetail, ReviewReport } from "../../api/types";
import { useReviewComments } from "../../hooks/useReviewComments";
import { useReviewFindings } from "../../hooks/useReviews";
import { findingsByAnnotationId } from "../../lib/diffFindings";
import { buildWorkOrder, disputedFindings, openQuestions, verdictsDisagree } from "../../lib/dialectic";
import { toast } from "../../lib/toast";

export interface DialecticLedgerProps {
  repo: string;
  reviewId: number;
  review: ReviewDetail;
  report: ReviewReport;
}

export default function DialecticLedger({ repo, reviewId, review, report }: DialecticLedgerProps) {
  const [open, setOpen] = useState(false);
  const disagree = verdictsDisagree(report.verdict, review.verdict?.state);

  // Only fetched once the strip has already decided the two voices clash —
  // same "don't pay for what you don't render" posture the rest of this
  // Room's lazy fetches follow (`useDiagnostics`/`useReviewImpact`).
  const findingsQ = useReviewFindings(repo, disagree ? reviewId : undefined, {});
  const commentsQ = useReviewComments(repo, disagree ? reviewId : undefined, "latest", true);

  const findingsById = useMemo(
    () => findingsByAnnotationId(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );
  const disputed = useMemo(
    () => disputedFindings(repo, reviewId, findingsQ.data?.findings ?? []),
    [repo, reviewId, findingsQ.data],
  );
  const questions = useMemo(
    () => (commentsQ.data ? openQuestions(repo, reviewId, commentsQ.data, findingsById) : []),
    [repo, reviewId, commentsQ.data, findingsById],
  );

  if (!disagree) return null;

  async function onHandToAgent() {
    const text = buildWorkOrder(window.location.origin, disputed, questions);
    try {
      await navigator.clipboard.writeText(text);
      toast.ok("Work order copied — paste it to the agent");
    } catch {
      toast.err("couldn't copy — clipboard unavailable");
    }
  }

  return (
    <div className="kbc-dialectic" data-kbc-dialectic>
      <button
        type="button"
        className="kbc-dialectic__toggle"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        data-kbc-dialectic-toggle
      >
        you and the agent disagree — {open ? "hide" : "points of disagreement"}
      </button>
      {open && (
        <div className="kbc-dialectic__body" data-kbc-dialectic-body>
          {disputed.length === 0 && questions.length === 0 ? (
            <p className="kbc-review__card-empty" data-kbc-dialectic-empty>
              No disputed findings or open questions to hand over yet.
            </p>
          ) : (
            <>
              {disputed.map((d) => (
                <div key={d.slug} className="kbc-dialectic__item" data-kbc-dialectic-finding={d.slug}>
                  <a href={d.href} className="kbc-dialectic__slug" data-kbc-dialectic-finding-link={d.slug}>
                    {d.slug}
                  </a>
                  <span className="kbc-dialectic__title">{d.title}</span>
                  <p className="kbc-dialectic__voice kbc-dialectic__voice--agent">
                    <strong>agent:</strong> {d.agentWord}
                  </p>
                  <p className="kbc-dialectic__voice kbc-dialectic__voice--human">
                    <strong>you:</strong> {d.humanWord ?? "(no note left)"}
                  </p>
                </div>
              ))}
              {questions.length > 0 && (
                <div className="kbc-dialectic__questions" data-kbc-dialectic-questions>
                  <h3 className="kbc-review__card-title">Open questions ({questions.length})</h3>
                  {questions.map((q) => (
                    <a key={q.id} href={q.href} className="kbc-dialectic__question" data-kbc-dialectic-question={q.id}>
                      {q.path || "general"}: {q.body}
                    </a>
                  ))}
                </div>
              )}
            </>
          )}
          <button
            type="button"
            className="kbc-btn kbc-btn--ghost"
            onClick={() => void onHandToAgent()}
            data-kbc-dialectic-hand-to-agent
          >
            Hand to agent
          </button>
        </div>
      )}
    </div>
  );
}
