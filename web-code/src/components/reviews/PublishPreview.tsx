// PRR-U5+U6 (design-ui.md §2 S5 — publish preview, MUST). The manual
// GitHub round's cockpit half: a `--z-modal` dialog (native `<dialog>`,
// same idiom as `StartReviewDialog.tsx`/`ConfirmModal.tsx`) that fetches
// `GET /api/reviews/{id}/export/github?finding_slugs=<marked>` and renders
// the exact publishable set — the SPA composes text, it never talks to
// GitHub itself (module doc, root CLAUDE.md's non-goal list).
//
// `finding_slugs` is ALWAYS the marked set, never omitted/empty: an absent
// `finding_slugs` on the server returns EVERY eligible finding, which would
// silently show "everything" instead of "what you marked" the moment a
// user opened this with zero marks (e.g. via `?publish=1` before marking
// anything) — so a zero-mark open renders its own empty state and never
// calls the export route at all.
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { ReviewDetailPr } from "../../api/types";
import { usePublishFinding, usePublishVerdict, useReviewExportGithub } from "../../hooks/useReviews";
import { clearMarks, useMarkedSlugs } from "../../lib/publishMarks";
import { composeGhCommandsText, type GhExportInput } from "../../lib/publishGithub";
import { toast } from "../../lib/toast";
import { Icon } from "../icons";
import { isLoopbackRefusal, LOOPBACK_HINT, msg } from "./ReviewHeader";

// Session-wide latch, same idiom as `DispositionMenu.tsx`/`AskAgentCard.tsx`
// — one 404 from either publish-recording route hides "Mark round done" for
// the rest of this tab's session. "Copy gh commands" is a pure client
// action (no network call) and is NEVER gated by this latch.
let publishLoopbackLatched = false;
const publishLoopbackListeners = new Set<() => void>();
function latchPublishLoopback() {
  if (publishLoopbackLatched) return;
  publishLoopbackLatched = true;
  for (const l of publishLoopbackListeners) l();
}
function usePublishLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      publishLoopbackListeners.add(cb);
      return () => publishLoopbackListeners.delete(cb);
    },
    () => publishLoopbackLatched,
    () => publishLoopbackLatched,
  );
}
/// Test-only escape hatch, same convention as `resetDispositionLoopbackLatchForTests`.
export function __resetPublishLoopbackLatchForTests(): void {
  publishLoopbackLatched = false;
}

export interface PublishPreviewProps {
  repo: string;
  reviewId: number;
  review: ReviewDetailPr;
  onClose: () => void;
}

const EVENT_LABEL: Record<string, string> = {
  APPROVE: "APPROVE",
  REQUEST_CHANGES: "REQUEST_CHANGES",
  COMMENT: "COMMENT",
};

export default function PublishPreview({ repo, reviewId, review, onClose }: PublishPreviewProps) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  const markedSlugs = useMarkedSlugs(reviewId);
  const [includeOrphanedAsGeneral, setIncludeOrphanedAsGeneral] = useState(false);
  const loopback = usePublishLoopbackLatched();

  const hasMarks = markedSlugs.length > 0;
  const exportQ = useReviewExportGithub(
    repo,
    reviewId,
    { finding_slugs: markedSlugs.join(","), include_orphaned_as_general: includeOrphanedAsGeneral },
    hasMarks,
  );

  const publishFindingM = usePublishFinding(repo);
  const publishVerdictM = usePublishVerdict(repo);
  const [markingDone, setMarkingDone] = useState(false);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  const data = exportQ.data;
  const prBound = review.pr_number != null && !!review.pr_repo_slug;

  async function onCopyCommands() {
    if (!data || !prBound) return;
    const input: GhExportInput = {
      ownerRepoSlug: review.pr_repo_slug as string,
      prNumber: review.pr_number as number,
      commitId: data.commit_id,
      event: data.event,
      verdictBody: data.body,
      comments: data.comments,
      generalComments: data.general_comments,
    };
    const text = composeGhCommandsText(input);
    try {
      await navigator.clipboard.writeText(text);
      toast.ok(`Copied ${input.comments.length + input.generalComments.length + (input.event ? 1 : 0)} command(s)`);
    } catch {
      toast.err("couldn't copy — clipboard unavailable");
    }
  }

  async function onMarkDone() {
    if (loopback || markingDone || !data) return;
    setMarkingDone(true);
    const slugs = [
      ...data.comments.map((c) => c.finding_slug),
      ...data.general_comments.map((g) => g.finding_slug),
    ];
    try {
      for (const slug of slugs) {
        await publishFindingM.mutateAsync({ id: reviewId, slug });
      }
      if (data.event) {
        await publishVerdictM.mutateAsync({ id: reviewId });
      }
      clearMarks(reviewId);
      toast.ok(`Marked ${slugs.length} item${slugs.length === 1 ? "" : "s"} as published${data.event ? " + verdict" : ""}`);
      onClose();
    } catch (e) {
      if (isLoopbackRefusal(e)) {
        latchPublishLoopback();
        return;
      }
      toast.err(`couldn't record publish: ${msg(e)}`);
    } finally {
      setMarkingDone(false);
    }
  }

  return (
    <dialog
      ref={dlgRef}
      className="confirm kbc-publish-preview"
      aria-labelledby="kbc-publish-preview-title"
      data-kbc-publish-preview
    >
      <h2 id="kbc-publish-preview-title" className="confirm__title">
        Publish to GitHub — preview
      </h2>
      <div className="confirm__body kbc-publish-preview__body">
        {!hasMarks ? (
          <p className="kbc-review__card-empty" data-kbc-publish-preview-empty>
            No findings marked yet — mark findings on the Report tab, then preview the round.
          </p>
        ) : !prBound ? (
          <p className="kbc-review__card-empty" data-kbc-publish-preview-no-pr>
            This review isn't bound to a GitHub PR — nothing to publish to.
          </p>
        ) : exportQ.isLoading ? (
          <div className="kbc-skeleton" style={{ height: 120 }} data-kbc-publish-preview-loading />
        ) : exportQ.error ? (
          <p className="kbc-review__card-empty" data-kbc-publish-preview-error>
            {(exportQ.error as Error).message}
          </p>
        ) : data ? (
          <>
            {data.stale_export && (
              <div className="kbc-stale-banner kbc-publish-preview__stale" role="alert" data-kbc-publish-stale>
                <span className="glyph" aria-hidden="true">
                  ⚠
                </span>
                <span>
                  The PR head has moved past this review's latest patchset — publishing now will comment
                  against a stale commit. Fetch the new head and re-snapshot before publishing.
                </span>
              </div>
            )}

            <div className="kbc-publish-preview__verdict" data-kbc-publish-verdict-mapping>
              <span className="l">Verdict mapping</span>
              {data.event ? (
                <span className="v">
                  {review.verdict?.state ?? "?"} → <code>{EVENT_LABEL[data.event]}</code>
                </span>
              ) : (
                <span className="v kbc-publish-preview__no-verdict">
                  no verdict set — set one in the header to publish a GitHub review event
                  {data.event_reason ? ` (${data.event_reason})` : ""}
                </span>
              )}
            </div>

            <div className="kbc-publish-preview__items" data-kbc-publish-items>
              <span className="kbc-eyebrow">
                {data.comments.length + data.general_comments.length} item
                {data.comments.length + data.general_comments.length === 1 ? "" : "s"} marked
              </span>
              <ul className="kbc-publish-preview__list">
                {data.comments.map((c) => (
                  <li key={c.finding_slug} data-kbc-publish-item={c.finding_slug}>
                    <span className="kbc-finding__slug">{c.finding_slug}</span>
                    <code>
                      {c.path}:{c.line}
                      {c.line_end != null ? `-${c.line_end}` : ""}
                    </code>
                    <span className="side">{c.side}</span>
                  </li>
                ))}
                {data.general_comments.map((g) => (
                  <li key={g.finding_slug} data-kbc-publish-item={g.finding_slug}>
                    <span className="kbc-finding__slug">{g.finding_slug}</span>
                    <span className="general">general (file-level)</span>
                  </li>
                ))}
              </ul>
            </div>

            {data.skipped_orphaned.length > 0 && (
              <div className="kbc-publish-preview__skipped" data-kbc-publish-skipped>
                <span className="kbc-eyebrow">
                  {data.skipped_orphaned.length} marked item{data.skipped_orphaned.length === 1 ? " is" : "s are"}{" "}
                  orphaned/imprecise — excluded from inline comments
                </span>
                <ul>
                  {data.skipped_orphaned.map((s) => (
                    <li key={s.finding_slug}>
                      {s.finding_slug} — {s.reason} (was ps{s.original.ps}
                      {s.original.line != null ? `:${s.original.line}` : ""})
                    </li>
                  ))}
                </ul>
                <label className="kbc-publish-preview__toggle">
                  <input
                    type="checkbox"
                    checked={includeOrphanedAsGeneral}
                    onChange={(e) => setIncludeOrphanedAsGeneral(e.target.checked)}
                    data-kbc-publish-include-orphaned
                  />
                  publish these file-level instead (opt-in)
                </label>
              </div>
            )}

            <div className="kbc-report-empty__commands kbc-publish-preview__hint" data-kbc-publish-cli-hint>
              $ kb-code review publish {reviewId} --dry-run
              <br />
              or hand this round to the agent — it reads marks via `review distill`.
            </div>
          </>
        ) : null}
      </div>
      <div className="confirm__actions kbc-publish-preview__actions">
        <button type="button" className="confirm__cancel" onClick={onClose}>
          Close
        </button>
        {hasMarks && prBound && data && (
          <>
            <button
              type="button"
              className="kbc-btn"
              onClick={() => void onCopyCommands()}
              data-kbc-publish-copy-commands
            >
              <Icon.Copy /> Copy gh commands
            </button>
            {loopback ? (
              <span className="kbc-finding__foot-loopback" title={LOOPBACK_HINT} data-kbc-publish-mark-done-loopback>
                {LOOPBACK_HINT}
              </span>
            ) : (
              <button
                type="button"
                className="confirm__go"
                onClick={() => void onMarkDone()}
                disabled={markingDone}
                data-kbc-publish-mark-done
              >
                {markingDone ? "Marking…" : "Mark round done"}
              </button>
            )}
          </>
        )}
      </div>
    </dialog>
  );
}
