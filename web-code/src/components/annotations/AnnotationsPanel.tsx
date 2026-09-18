import { useEffect, useState } from "react";
import { Link } from "react-router";
import { ApiError, type BindAnnotationReviewInput } from "../../api/client";
import type { AnchorKind, AnnotationIntent, AnnotationView } from "../../api/types";
import {
  useAnnotations,
  useBindAnnotationReview,
  useCreateAnnotation,
  useDeleteAnnotation,
  usePatchAnnotation,
  useUnbindAnnotationReview,
} from "../../hooks/useAnnotations";
import { useReviews } from "../../hooks/useReviews";
import {
  anchorBadgeLabel,
  buildCreatePayload,
  buildReplyPayload,
  groupThreads,
  INTENT_OPTIONS,
  intentLabel,
  type AnnotationThread,
} from "../../lib/annotations";
import { commitUrl, reviewUrl } from "../../lib/codeUrl";
import { useCurrentReview } from "../../lib/currentReview";
import { formatUnixSeconds, shortSha } from "../../lib/format";
import { toast } from "../../lib/toast";
import { useConfirm } from "../ConfirmProvider";
import IntentChip from "./IntentChip";
import ReviewBindSelector from "./ReviewBindSelector";

export interface AnnotationsPanelProps {
  repo: string;
  path: string;
  /// The line the gutter "+" affordance / `a` keybinding last targeted —
  /// prefills the composer's line field (still editable). `null` leaves
  /// whatever the composer already has.
  activeLine: number | null;
  /// Phase D — the OTHER end of a visual-mode `a` selection (`Reader.tsx`'s
  /// vim callback now forwards `sel.lineEnd`, previously dropped on the
  /// floor). `null` means "no range" — a plain single-line composer. When
  /// set, the composer shows a read-only `L{start}–{end}` badge instead of
  /// an editable line-number input and posts a `range` annotation.
  activeLineEnd: number | null;
  /// V71-E2 — the intent the composer opens ON. `kbc-actions/1`'s "Ask
  /// here" is a required row for every target kind, and a row that opens a
  /// NOTE composer has not delivered a question channel. Absent ⇒ `"note"`,
  /// exactly as before.
  initialIntent?: AnnotationIntent;
  /// `lineEnd` is `undefined` for a plain line/symbol jump; set for a
  /// `range` badge click, which selects the WHOLE span via the existing
  /// CM6 `gotoSel` machinery (`Reader.tsx`'s `jumpToLine`).
  onGotoLine: (line: number, lineEnd?: number) => void;
}

type IntentFilterKey = "all" | "flag-for-agent" | "question" | "todo";

/// Deliberately not the full 5-intent vocabulary — `note`/`tour-stop` are
/// the low-signal defaults; the filter row surfaces the three ACTIONABLE
/// intents (per the phase brief: "All | flagged | questions | todos").
const FILTERS: { key: IntentFilterKey; label: string }[] = [
  { key: "all", label: "All" },
  { key: "flag-for-agent", label: "Flagged" },
  { key: "question", label: "Questions" },
  { key: "todo", label: "Todos" },
];

/// W4.6 UI, Phase D composer v2 — the right rail's annotations tab: a
/// composer (line/range + body + intent + an "attach to enclosing symbol"
/// toggle, `POST /api/annotations`) plus the file's existing annotations
/// grouped into THREADS (a parent + its replies, `lib/annotations.ts`'s
/// `groupThreads`), each thread showing an anchor-kind badge, an intent
/// chip, resolve/reopen (resolves the whole thread visually), delete, and
/// an inline reply composer. An intent filter row narrows the list to one
/// actionable intent at a time. Kept fresh by `api/queryClient.ts`'s
/// `annotation.changed` SSE handler, not polling.
export default function AnnotationsPanel({
  repo,
  path,
  activeLine,
  activeLineEnd,
  initialIntent,
  onGotoLine,
}: AnnotationsPanelProps) {
  const { data, isLoading } = useAnnotations(repo, path);
  const create = useCreateAnnotation(repo, path);
  const patch = usePatchAnnotation(repo, path);
  const del = useDeleteAnnotation(repo, path);
  const bindReviewMut = useBindAnnotationReview(repo, path);
  const unbindReviewMut = useUnbindAnnotationReview(repo, path);
  const confirm = useConfirm();

  // V80-M2 — every open review's title, for the card chip's label + the
  // composer's "will appear in the Room of <title>" hint (shared cache
  // with `ReviewBindSelector`'s own `useReviews` call, so this costs no
  // extra request). A row bound to a since-closed review just falls back
  // to `#<id>` — the same honest degrade `lib/currentReview.ts` documents
  // for its own title-less marker.
  const openReviews = useReviews(repo, "open");
  function reviewTitleFor(id: number): string | undefined {
    return openReviews.data?.reviews.find((r) => r.id === id)?.title ?? undefined;
  }

  // V80-M2 — the composer's "Review" selector preselects to the current
  // review (`lib/currentReview.ts`, M3) on mount. A one-time seed, not a
  // standing sync: this component instance persists across a file switch
  // within the same reader session (`InspectorRail` never remounts it on
  // `path` alone), and re-clobbering an operator's deliberate mid-session
  // choice every time the ambient marker changes elsewhere (a Room visit
  // in another tab, say) would be more surprising than a preselection that
  // only applies once, fresh.
  const currentReview = useCurrentReview(repo);
  const [reviewId, setReviewId] = useState<number | null>(() => {
    const n = currentReview ? Number(currentReview.id) : NaN;
    return Number.isFinite(n) ? n : null;
  });

  const [line, setLine] = useState<number>(activeLine ?? 1);
  const [lineEnd, setLineEnd] = useState<number | null>(activeLineEnd);
  const [attachSymbol, setAttachSymbol] = useState(false);
  const [intent, setIntent] = useState<AnnotationIntent>(initialIntent ?? "note");
  // V71-E2 — `kbc-actions/1`'s "Ask here" row is a REQUIRED action on every
  // target kind, so it has to land on a composer that is already a QUESTION.
  // Keyed on `initialIntent` ALONE, deliberately: the caller flips it to
  // `"question"` for Ask-here and back to `undefined` for the plain `a`
  // door, so both directions re-seed exactly once — and an operator who
  // changed the select by hand mid-composition keeps their choice, because
  // moving the active LINE is not in this dependency list.
  useEffect(() => {
    setIntent(initialIntent ?? "note");
  }, [initialIntent]);
  const [body, setBody] = useState("");
  const [filter, setFilter] = useState<IntentFilterKey>("all");

  useEffect(() => {
    if (activeLine !== null) setLine(activeLine);
  }, [activeLine]);
  useEffect(() => {
    setLineEnd(activeLineEnd);
    // A range and a symbol anchor are mutually exclusive server-side (an
    // annotation is exactly one `anchor_kind`) — a fresh range selection
    // always wins over whatever the toggle was left at.
    if (activeLineEnd !== null) setAttachSymbol(false);
  }, [activeLineEnd]);

  const isRange = lineEnd !== null;
  const anchorKind: AnchorKind = isRange ? "range" : attachSymbol ? "symbol" : "line";

  const threads = data ? groupThreads(data.annotations) : [];
  const visibleThreads = filter === "all" ? threads : threads.filter((t) => t.parent.intent === filter);

  // V80-M2 — splice the selected review onto an otherwise-plain create
  // payload. `ps` is deliberately omitted (→ server default, the review's
  // latest); `side` is always `"new"` — this composer only ever anchors to
  // the CURRENT working-tree content, never a historical diff side.
  function withReviewScope(p: ReturnType<typeof buildCreatePayload>) {
    if (!p || reviewId === null) return p;
    return { ...p, review_id: reviewId, side: "new" as const };
  }

  const payload = withReviewScope(
    buildCreatePayload({
      repo,
      path,
      line,
      lineEnd: isRange ? (lineEnd ?? undefined) : undefined,
      body,
      anchorKind,
      intent,
    }),
  );

  // F3a — every mutation here `await`s `…mutateAsync(...)` wrapped in a
  // try/catch so a rejection (a stale ETag, a network blip) always surfaces
  // via `toast.err` rather than becoming a silent unhandled rejection.
  async function save() {
    if (!payload) return;
    try {
      await create.mutateAsync(payload);
      setBody("");
      setAttachSymbol(false);
      setIntent("note");
    } catch (e) {
      // The server's honest "no enclosing symbol at this line" 404 (see
      // `routes::create_annotation`'s doc) — never lose the operator's
      // already-typed text: fall back to an ordinary `line` annotation
      // automatically, and say so.
      if (anchorKind === "symbol" && e instanceof ApiError && e.status === 404) {
        toast.warn("no enclosing symbol here — saved as a line annotation instead");
        const fallback = withReviewScope(buildCreatePayload({ repo, path, line, body, anchorKind: "line", intent }));
        if (fallback) {
          try {
            await create.mutateAsync(fallback);
            setBody("");
            setAttachSymbol(false);
            setIntent("note");
          } catch (e2) {
            toast.err(`couldn't save annotation: ${e2 instanceof Error ? e2.message : String(e2)}`);
          }
        }
        return;
      }
      toast.err(`couldn't save annotation: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function toggleResolved(id: string, resolved: boolean) {
    try {
      await patch.mutateAsync({ id, input: { resolved: !resolved } });
    } catch (e) {
      toast.err(`couldn't update annotation: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function remove(id: string) {
    const ok = await confirm({
      title: "Delete this annotation?",
      body: "This can't be undone.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await del.mutateAsync(id);
    } catch (e) {
      toast.err(`couldn't delete annotation: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /// V80-M2 — bind, or rebind onto a different review, an EXISTING
  /// annotation. Returns whether it actually applied, so `ThreadItem`
  /// closes its inline picker only on success (a rejection — most likely
  /// the server's honest 409 on a closed target review — leaves the
  /// picker open to retry or pick something else).
  async function bindToReview(id: string, input: BindAnnotationReviewInput): Promise<boolean> {
    try {
      await bindReviewMut.mutateAsync({ id, input });
      return true;
    } catch (e) {
      toast.err(`couldn't bind to review: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
  }

  async function unbindFromReview(id: string) {
    try {
      await unbindReviewMut.mutateAsync(id);
    } catch (e) {
      toast.err(`couldn't unbind from review: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /// Returns whether the reply actually posted — `ThreadItem` clears its
  /// own local composer state only on success, so a failed reply leaves
  /// the operator's text in place to retry.
  async function reply(parentId: string, replyBody: string): Promise<boolean> {
    const replyPayload = buildReplyPayload(repo, path, parentId, replyBody);
    if (!replyPayload) return false;
    try {
      await create.mutateAsync(replyPayload);
      return true;
    } catch (e) {
      toast.err(`couldn't post reply: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
  }

  return (
    <div className="kbc-annotations">
      <div className="kbc-annotations__composer">
        <div className="kbc-annotations__composer-row">
          {isRange ? (
            <span className="kbc-annotations__range-badge" data-kbc-annot-range-badge>
              L{line}–{lineEnd}
            </span>
          ) : (
            <input
              type="number"
              min={1}
              className="kbc-annotations__line-input"
              value={line}
              onChange={(e) => setLine(parseInt(e.target.value, 10) || 1)}
              aria-label="annotation line number"
              data-kbc-annot-line-input
            />
          )}
          <textarea
            className="kbc-annotations__body-input"
            placeholder="Add an annotation…"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            aria-label="annotation body"
            data-kbc-annot-body-input
            rows={2}
          />
        </div>
        <div className="kbc-annotations__composer-options">
          <label className="kbc-annotations__symbol-toggle" title="Anchor to the function/struct/etc. enclosing this line, not the line itself">
            <input
              type="checkbox"
              checked={attachSymbol}
              disabled={isRange}
              onChange={(e) => setAttachSymbol(e.target.checked)}
              data-kbc-annot-symbol-toggle
            />
            Attach to enclosing symbol
          </label>
          <select
            className="kbc-annotations__intent-select"
            value={intent}
            onChange={(e) => setIntent(e.target.value as AnnotationIntent)}
            aria-label="annotation intent"
            data-kbc-annot-intent-select
          >
            {INTENT_OPTIONS.map((i) => (
              <option key={i} value={i}>
                {intentLabel(i)}
              </option>
            ))}
          </select>
        </div>
        <ReviewBindSelector repo={repo} path={path} value={reviewId} onChange={setReviewId} />
        <button
          type="button"
          className="kbc-annotations__save"
          disabled={!payload || create.isPending}
          onClick={save}
          data-kbc-annot-save
        >
          {create.isPending ? "Saving…" : "Save"}
        </button>
      </div>

      <div className="kbc-annotations__filters" role="group" aria-label="filter annotations by intent">
        {FILTERS.map((f) => (
          <button
            key={f.key}
            type="button"
            className={"kbc-annotations__filter" + (filter === f.key ? " is-on" : "")}
            onClick={() => setFilter(f.key)}
            data-kbc-annot-filter={f.key}
          >
            {f.label}
          </button>
        ))}
      </div>

      {isLoading ? (
        <div className="kbc-annotations__empty">Loading…</div>
      ) : visibleThreads.length === 0 ? (
        <div className="kbc-annotations__empty">
          {threads.length === 0 ? "No annotations on this file yet." : "No annotations match this filter."}
        </div>
      ) : (
        <ul className="kbc-annotations__list">
          {visibleThreads.map((thread) => (
            <ThreadItem
              key={thread.parent.id}
              repo={repo}
              thread={thread}
              onGotoLine={onGotoLine}
              onToggleResolved={toggleResolved}
              onDelete={remove}
              onReply={reply}
              reviewTitleFor={reviewTitleFor}
              onBindReview={bindToReview}
              onUnbindReview={unbindFromReview}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

interface ThreadItemProps {
  repo: string;
  thread: AnnotationThread;
  onGotoLine: (line: number, lineEnd?: number) => void;
  onToggleResolved: (id: string, resolved: boolean) => void;
  onDelete: (id: string) => void;
  onReply: (parentId: string, body: string) => Promise<boolean>;
  /// V80-M2 — resolves an open review's title for the binding chip's
  /// label; `undefined` for a since-closed (or otherwise not-currently-
  /// open) review, which the chip degrades to `#<id>` for.
  reviewTitleFor: (id: number) => string | undefined;
  onBindReview: (id: string, input: BindAnnotationReviewInput) => Promise<boolean>;
  onUnbindReview: (id: string) => Promise<void>;
}

/// One thread: the parent row (anchor badge, body, intent chip, meta,
/// resolve/reopen/reply/delete) plus its replies indented one level under
/// it, plus an inline reply composer that opens on demand. Resolving the
/// parent visually resolves the WHOLE thread (`.is-resolved` on the `<li>`
/// root) — a reply is never independently resolved through this UI.
///
/// V80-M2 — the parent ALSO carries its review binding: a chip linking to
/// the Room when `review_id` is set, plus "bind to review…"/"rebind"
/// (opens an inline `ReviewBindSelector`) and "unbind". A REPLY has no
/// scope of its own (`routes::bind_annotation_review`'s doc — it always
/// inherits its parent's), so none of this renders on a reply row.
function ThreadItem({
  repo,
  thread,
  onGotoLine,
  onToggleResolved,
  onDelete,
  onReply,
  reviewTitleFor,
  onBindReview,
  onUnbindReview,
}: ThreadItemProps) {
  const { parent, replies } = thread;
  const [replyOpen, setReplyOpen] = useState(false);
  const [replyBody, setReplyBody] = useState("");
  const [replyPending, setReplyPending] = useState(false);
  const [bindPickerOpen, setBindPickerOpen] = useState(false);
  const [bindPickerValue, setBindPickerValue] = useState<number | null>(parent.review_id ?? null);
  const [bindPending, setBindPending] = useState(false);

  async function saveReply() {
    if (!replyBody.trim() || replyPending) return;
    setReplyPending(true);
    const ok = await onReply(parent.id, replyBody);
    setReplyPending(false);
    if (ok) {
      setReplyBody("");
      setReplyOpen(false);
    }
  }

  function openBindPicker() {
    setBindPickerValue(parent.review_id ?? null);
    setBindPickerOpen(true);
  }

  /// "No review" in the picker is a valid choice — same effect as the
  /// dedicated Unbind button, just reached from inside the picker instead.
  async function applyBindPicker() {
    setBindPending(true);
    const ok =
      bindPickerValue === null
        ? await (async () => {
            await onUnbindReview(parent.id);
            return true;
          })()
        : await onBindReview(parent.id, { review_id: bindPickerValue });
    setBindPending(false);
    if (ok) setBindPickerOpen(false);
  }

  async function unbindNow() {
    setBindPending(true);
    await onUnbindReview(parent.id);
    setBindPending(false);
  }

  return (
    <li className={"kbc-annotations__thread" + (parent.resolved ? " is-resolved" : "")} data-kbc-annot-id={parent.id}>
      <div className="kbc-annotations__item">
        <AnchorBadge repo={repo} annotation={parent} onGotoLine={onGotoLine} />
        <div className="kbc-annotations__item-body">
          <p className={"kbc-annotations__text" + (parent.resolved ? " kbc-annotations__text--resolved" : "")}>
            {parent.body}
          </p>
          <div className="kbc-annotations__item-meta">
            <IntentChip intent={parent.intent} />
            <span>
              {parent.author} · {formatUnixSeconds(parent.updated_at)}
            </span>
            {parent.stale && (
              <span className="kbc-annotations__badge kbc-annotations__badge--stale" data-kbc-annot-stale>
                stale
              </span>
            )}
            {parent.resolved && (
              <span className="kbc-annotations__badge kbc-annotations__badge--resolved">resolved</span>
            )}
            {parent.review_id != null && (
              <Link
                to={reviewUrl(repo, parent.review_id)}
                className="kbc-annotations__review-chip"
                data-kbc-annot-review-chip
              >
                in review {reviewTitleFor(parent.review_id) ?? `#${parent.review_id}`}
              </Link>
            )}
          </div>
          {bindPickerOpen && (
            <div className="kbc-annotations__review-picker-inline" data-kbc-annot-review-picker>
              <ReviewBindSelector repo={repo} path={parent.path} value={bindPickerValue} onChange={setBindPickerValue} />
              <div className="kbc-annotations__review-picker-actions">
                <button
                  type="button"
                  disabled={bindPending}
                  onClick={() => void applyBindPicker()}
                  data-kbc-annot-review-apply
                >
                  {bindPending ? "Saving…" : "Save"}
                </button>
                <button type="button" onClick={() => setBindPickerOpen(false)} data-kbc-annot-review-cancel>
                  Cancel
                </button>
              </div>
            </div>
          )}
        </div>
        <div className="kbc-annotations__item-actions">
          <button type="button" onClick={() => onToggleResolved(parent.id, parent.resolved)} data-kbc-annot-resolve>
            {parent.resolved ? "Reopen" : "Resolve"}
          </button>
          <button type="button" onClick={() => setReplyOpen((v) => !v)} data-kbc-annot-reply-toggle>
            Reply{replies.length > 0 ? ` (${replies.length})` : ""}
          </button>
          <button type="button" onClick={openBindPicker} data-kbc-annot-review-toggle>
            {parent.review_id != null ? "Rebind" : "Bind to review…"}
          </button>
          {parent.review_id != null && (
            <button type="button" disabled={bindPending} onClick={() => void unbindNow()} data-kbc-annot-review-unbind>
              Unbind
            </button>
          )}
          <button
            type="button"
            className="kbc-annotations__delete"
            onClick={() => onDelete(parent.id)}
            data-kbc-annot-delete
          >
            Delete
          </button>
        </div>
      </div>

      {replies.length > 0 && (
        <ul className="kbc-annotations__replies" data-kbc-annot-replies={parent.id}>
          {replies.map((r) => (
            <li key={r.id} className="kbc-annotations__reply" data-kbc-annot-reply-id={r.id}>
              <p className="kbc-annotations__text">{r.body}</p>
              <span className="kbc-annotations__reply-meta">
                {r.author} · {formatUnixSeconds(r.updated_at)}
              </span>
            </li>
          ))}
        </ul>
      )}

      {replyOpen && (
        <div className="kbc-annotations__reply-composer">
          <input
            type="text"
            className="kbc-annotations__reply-input"
            placeholder="Reply…"
            value={replyBody}
            onChange={(e) => setReplyBody(e.target.value)}
            aria-label="reply body"
            data-kbc-annot-reply-input
            onKeyDown={(e) => {
              if (e.key === "Enter") void saveReply();
            }}
          />
          <button
            type="button"
            disabled={!replyBody.trim() || replyPending}
            onClick={() => void saveReply()}
            data-kbc-annot-reply-save
          >
            {replyPending ? "Posting…" : "Post"}
          </button>
        </div>
      )}
    </li>
  );
}

/// The per-item anchor-kind badge/goto affordance. `line`/`symbol` jump to
/// their live-resolved `line` (a symbol annotation "follows drift" purely
/// because the SERVER re-derives that line on every fetch — this is just a
/// plain jump, see `crate::annotations::resolve_symbol`'s doc); `range`
/// jumps to the whole `[line, line_end]` span via the SAME `onGotoLine`,
/// extended with an optional end that drives CM6's existing range-select
/// (`Reader.tsx`'s `gotoSel`). `diff` has no live position to jump to at
/// all — pinned to one immutable commit blob (`crate::annotations::
/// diff_line`'s doc) — so it renders as a short-sha chip linking to the
/// commit page instead of a jump button.
function AnchorBadge({
  repo,
  annotation,
  onGotoLine,
}: {
  repo: string;
  annotation: AnnotationView;
  onGotoLine: (line: number, lineEnd?: number) => void;
}) {
  if (annotation.anchor_kind === "diff" && annotation.sha) {
    return (
      <Link
        to={commitUrl(repo, annotation.sha)}
        className="kbc-annotations__goto kbc-annotations__sha-chip"
        title={`Pinned at commit ${annotation.sha}`}
        data-kbc-annot-sha-chip
      >
        {shortSha(annotation.sha)}
      </Link>
    );
  }
  const label = anchorBadgeLabel(annotation);
  const kindClass =
    annotation.anchor_kind === "range"
      ? " kbc-annotations__goto--range"
      : annotation.anchor_kind === "symbol"
        ? " kbc-annotations__goto--symbol"
        : "";
  return (
    <button
      type="button"
      className={"kbc-annotations__goto" + kindClass}
      onClick={() => onGotoLine(annotation.line, annotation.line_end ?? undefined)}
      title={annotation.anchor_kind === "symbol" ? "Go to symbol" : "Go to line"}
      data-kbc-annot-goto
    >
      {label}
    </button>
  );
}
