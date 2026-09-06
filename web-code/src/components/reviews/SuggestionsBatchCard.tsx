// PRR-U5+U6 (addendum-2 §F UI — suggestions batch card). Side-panel card:
// every unapplied suggestion across the review (from the SAME `useReview
// Comments` cache `ReviewThreadsCard`/`ReviewSidePanel` already fetch —
// TanStack Query dedups by key, no extra round-trip), a confirm-gated
// "Apply N suggestions" → `POST /api/annotations/apply-batch`
// (LOOPBACK-ONLY). A 409 renders the per-id verify-phase verdicts inline
// (nothing was written); success toasts a per-item summary.
import { useState, useSyncExternalStore } from "react";
import { ApplyBatchConflictError, ApplyBatchWriteError } from "../../api/client";
import { useApplySuggestionsBatch } from "../../hooks/useReviews";
import { useReviewComments } from "../../hooks/useReviewComments";
import type { ApplyBatchVerdict } from "../../api/types";
import {
  failingVerdicts,
  unappliedSuggestionById,
  unappliedSuggestionRows,
} from "../../lib/suggestionsBatch";
import { toast } from "../../lib/toast";
import { useConfirm } from "../ConfirmProvider";
import { Icon } from "../icons";
import { isLoopbackRefusal, LOOPBACK_HINT, msg } from "./ReviewHeader";

// Session-wide latch — same idiom as `DispositionMenu.tsx`'s
// `dispositionLoopbackLatched`: one 404 from apply-batch hides the Apply
// button for the rest of this tab's session.
let suggestionsBatchLoopbackLatched = false;
const suggestionsBatchLoopbackListeners = new Set<() => void>();
function latchSuggestionsBatchLoopback() {
  if (suggestionsBatchLoopbackLatched) return;
  suggestionsBatchLoopbackLatched = true;
  for (const l of suggestionsBatchLoopbackListeners) l();
}
function useSuggestionsBatchLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      suggestionsBatchLoopbackListeners.add(cb);
      return () => suggestionsBatchLoopbackListeners.delete(cb);
    },
    () => suggestionsBatchLoopbackLatched,
    () => suggestionsBatchLoopbackLatched,
  );
}
/// Test-only escape hatch.
export function __resetSuggestionsBatchLoopbackLatchForTests(): void {
  suggestionsBatchLoopbackLatched = false;
}

export interface SuggestionsBatchCardProps {
  repo: string;
  reviewId: number;
  ps: string;
}

export default function SuggestionsBatchCard({ repo, reviewId, ps }: SuggestionsBatchCardProps) {
  const commentsQ = useReviewComments(repo, reviewId, ps, true);
  const confirm = useConfirm();
  const apply = useApplySuggestionsBatch(repo);
  const loopback = useSuggestionsBatchLoopbackLatched();
  const [resolveThreads, setResolveThreads] = useState(false);
  const [failed, setFailed] = useState<ApplyBatchVerdict[] | null>(null);

  if (!commentsQ.data) {
    return (
      <section className="kbc-review__card kbc-suggestions-batch" data-kbc-suggestions-batch>
        <h2 className="kbc-review__card-title">Suggestions</h2>
        <div className="kbc-skeleton" style={{ height: 40 }} />
      </section>
    );
  }

  const rows = unappliedSuggestionRows(commentsQ.data);
  const byId = unappliedSuggestionById(commentsQ.data);

  async function onApply() {
    if (loopback || apply.isPending || rows.length === 0) return;
    const ok = await confirm({
      title: `Apply ${rows.length} suggestion${rows.length === 1 ? "" : "s"}?`,
      body: "This writes directly to the working tree across every file listed below.",
      confirmLabel: "Apply",
      danger: true,
    });
    if (!ok) return;
    setFailed(null);
    try {
      const result = await apply.mutateAsync({ ids: rows.map((r) => r.id), resolveThreads });
      toast.ok(
        `Applied ${result.applied.length} suggestion${result.applied.length === 1 ? "" : "s"} across the review`,
      );
    } catch (e) {
      if (isLoopbackRefusal(e)) {
        latchSuggestionsBatchLoopback();
        return;
      }
      if (e instanceof ApplyBatchConflictError) {
        setFailed(failingVerdicts(e.verdicts));
        toast.err(`${failingVerdicts(e.verdicts).length} suggestion(s) failed verification — nothing was written`);
        return;
      }
      if (e instanceof ApplyBatchWriteError) {
        toast.err(
          `apply-batch failed mid-write on ${e.failed?.path ?? "a file"}: ${e.failed?.error ?? "unknown error"} (${e.restored.length} file(s) rolled back)`,
        );
        return;
      }
      toast.err(`couldn't apply suggestions: ${msg(e)}`);
    }
  }

  return (
    <section className="kbc-review__card kbc-suggestions-batch" data-kbc-suggestions-batch>
      <h2 className="kbc-review__card-title">
        Suggestions
        <span className="n">{rows.length}</span>
      </h2>
      {rows.length === 0 ? (
        <p className="kbc-review__card-empty" data-kbc-suggestions-batch-empty>
          No unapplied suggestions.
        </p>
      ) : (
        <>
          <ul className="kbc-suggestions-batch__list">
            {rows.map((r) => (
              <li key={r.id} data-kbc-suggestions-batch-row={r.id}>
                <code>
                  {r.path}
                  {r.line != null ? `:${r.line}` : ""}
                </code>
                <span className="preview">{r.preview}</span>
                {r.orphaned && (
                  <span className="orphan" title="anchor no longer resolves — apply will likely fail">
                    <Icon.Unlink />
                  </span>
                )}
              </li>
            ))}
          </ul>
          {failed && failed.length > 0 && (
            <div className="kbc-suggestions-batch__failed" data-kbc-suggestions-batch-failed>
              <span className="kbc-eyebrow">verification failed — nothing written</span>
              <ul>
                {failed.map((v) => {
                  const row = byId.get(v.id);
                  return (
                    <li key={v.id} data-kbc-suggestions-batch-verdict={v.id}>
                      <code>{row ? `${row.path}${row.resolution.line != null ? `:${row.resolution.line}` : ""}` : v.id}</code>
                      {" — "}
                      <span className="kind">{v.error?.kind ?? "unknown"}</span>
                      {v.error?.detail ? `: ${v.error.detail}` : ""}
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
          <label className="kbc-suggestions-batch__resolve">
            <input
              type="checkbox"
              checked={resolveThreads}
              onChange={(e) => setResolveThreads(e.target.checked)}
              data-kbc-suggestions-batch-resolve
            />
            resolve threads after applying
          </label>
          {loopback ? (
            <span
              className="kbc-finding__foot-loopback"
              title={LOOPBACK_HINT}
              data-kbc-suggestions-batch-loopback
            >
              {LOOPBACK_HINT}
            </span>
          ) : (
            <button
              type="button"
              className="kbc-btn kbc-btn--primary"
              onClick={() => void onApply()}
              disabled={apply.isPending}
              data-kbc-suggestions-batch-apply
            >
              {apply.isPending ? "Applying…" : `Apply ${rows.length} suggestion${rows.length === 1 ? "" : "s"}`}
            </button>
          )}
        </>
      )}
    </section>
  );
}
