import { useState } from "react";
import { fetchCodeActions, fetchFile, postAnnotationsBatch } from "../../api/client";
import type { CodeActionRow, CodeActionsOut } from "../../api/types";
import { readerUrl } from "../../lib/breadcrumbs";
import {
  actionToAnnotationOps,
  buildCodeActionsRequest,
  buildCodeActionsView,
  editTotalCount,
  type CodeActionsRange,
} from "../../lib/codeActions";
import { toast } from "../../lib/toast";

export interface QuickFixesProps {
  repo: string;
  path: string;
  /// The row/selection this "Fixes" affordance is scoped to — a
  /// `DiagnosticRow`'s own range (`lib/codeActions.ts`'s
  /// `rangeFromDiagnostic`), same convention `useDiagnostics` uses for its
  /// own (repo, path) scope.
  range: CodeActionsRange;
  /// Review scope, when this card is mounted inside a review-diff context
  /// (`routes/ReviewDiff.tsx`) rather than the plain working-tree Reader —
  /// threaded straight into the created annotation's `review_id`/`ps` so
  /// the resulting thread lands on the SAME review, not a bare working-tree
  /// comment. Both omitted for the Reader's own usage.
  reviewId?: number;
  ps?: number;
}

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/// S2-C — the quick-fixes affordance `DiagnosticsCard` rows carry. Since
/// `DiagnosticsCard` is the ONE shared component both `Reader.tsx`
/// (`InspectorRail`'s `diagnosticsCard` slot) and `routes/ReviewDiff.tsx`
/// (the diagnostics chip's on-demand expansion, `routes/ReviewDiff.tsx`'s
/// own doc) mount, this component automatically appears in both contexts —
/// there is no second copy to keep in sync.
///
/// On click: `POST /api/code-actions` for `range` (never eager — an
/// on-demand fetch per design-s2.md §S2-C, unlike `useDiagnostics`'s own
/// eager-on-file-open contract); renders the honest state matrix
/// (`lib/codeActions.ts`'s `buildCodeActionsView`: loading/reason/empty/
/// actions, `capability_absent` included in the closed vocabulary).
/// Clicking a listed action converts its `TextEdit`s into one
/// `add_comment(+suggestion)` op per (file, edit) and posts them through
/// the EXISTING atomic `POST /api/annotations/batch` — the audited
/// creation path, never a bespoke mutation route (design-s2.md §S2-C's own
/// "No new mutation route" line). A per-touched-file content fetch (best
/// effort — a failed fetch just degrades that file's ops to the edit's own
/// bare `new_text`, `editToAnnotationOp`'s documented fallback) lets the
/// splice math build a real full-line suggestion `replacement`. Applying a
/// created suggestion remains the EXISTING loopback-only suggestion UI —
/// this component only ever creates.
export default function QuickFixes({ repo, path, range, reviewId, ps }: QuickFixesProps) {
  const [open, setOpen] = useState(false);
  const [requested, setRequested] = useState(false);
  const [loading, setLoading] = useState(false);
  const [data, setData] = useState<CodeActionsOut | null>(null);
  const [fetchError, setFetchError] = useState<string | null>(null);
  const [creatingIdx, setCreatingIdx] = useState<number | null>(null);

  async function loadActions() {
    setLoading(true);
    setFetchError(null);
    try {
      const body = buildCodeActionsRequest(repo, path, range);
      const out = await fetchCodeActions(body);
      setData(out);
    } catch (e) {
      setFetchError(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  function onToggle() {
    const next = !open;
    setOpen(next);
    if (next && !requested) {
      setRequested(true);
      void loadActions();
    }
  }

  async function onConvert(action: CodeActionRow, idx: number) {
    setCreatingIdx(idx);
    try {
      const touchedPaths = [...new Set(action.edits.map((f) => f.path))];
      const fileContents: Record<string, string> = {};
      await Promise.all(
        touchedPaths.map(async (p) => {
          try {
            const file = await fetchFile(repo, p);
            fileContents[p] = file.content;
          } catch {
            // Best-effort — see this component's own doc: a failed fetch
            // for one touched file just falls back to that file's edits'
            // bare `new_text`, never blocks the whole conversion.
          }
        }),
      );
      const ops = actionToAnnotationOps(action, {
        provider: data?.provider,
        fileContents,
        reviewId,
        ps,
      });
      if (ops.length === 0) {
        toast.warn("Quick fix produced no edits to convert");
        return;
      }
      const result = await postAnnotationsBatch({ repo, ops });
      const firstOp = ops[0];
      const created = result.created_ids.length;
      toast.ok(`Created ${created} suggestion${created === 1 ? "" : "s"}`, {
        to: readerUrl(repo, firstOp.path, undefined, firstOp.line),
        label: "View thread",
      });
    } catch (e) {
      toast.err(`couldn't create suggestion: ${errMsg(e)}`);
    } finally {
      setCreatingIdx(null);
    }
  }

  const view = fetchError
    ? ({ kind: "reason" as const, reason: fetchError })
    : buildCodeActionsView(requested, loading, data);

  return (
    <div className="kbc-quickfixes" data-kbc-quickfixes>
      <button
        type="button"
        className="kbc-quickfixes__trigger"
        onClick={onToggle}
        aria-expanded={open}
        data-kbc-quickfixes-trigger
      >
        Fixes
      </button>
      {open && (
        <div className="kbc-quickfixes__panel" data-kbc-quickfixes-state={view.kind}>
          {view.kind === "loading" && (
            <p className="kbc-quickfixes__hint" data-kbc-quickfixes-loading>
              Loading fixes…
            </p>
          )}
          {view.kind === "reason" && (
            <p className="kbc-quickfixes__reason" data-kbc-quickfixes-reason>
              {view.reason}
            </p>
          )}
          {view.kind === "empty" && (
            <p className="kbc-quickfixes__hint" data-kbc-quickfixes-empty>
              No fixes available.
            </p>
          )}
          {view.kind === "actions" && view.actions && (
            <ul className="kbc-quickfixes__list">
              {view.actions.map((action, idx) => {
                const n = editTotalCount(action);
                return (
                  <li key={`${action.title}:${idx}`} className="kbc-quickfixes__row">
                    <button
                      type="button"
                      className="kbc-quickfixes__action"
                      disabled={creatingIdx !== null}
                      onClick={() => void onConvert(action, idx)}
                      data-kbc-quickfixes-action={idx}
                    >
                      <span className="kbc-quickfixes__title">{action.title}</span>
                      <span className="kbc-quickfixes__meta">
                        {n} edit{n === 1 ? "" : "s"}
                        {creatingIdx === idx ? " · creating…" : ""}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
