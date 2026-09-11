import { useEffect, useMemo, useState, useSyncExternalStore } from "react";
import { useParams } from "react-router-dom";
import { ApiError, ApplyConflictError } from "../../api/client";
import type { ReviewComment } from "../../api/types";
import { Icon } from "../icons";
import IntentChip from "../annotations/IntentChip";
import { useConfirm } from "../ConfirmProvider";
import { reviewDiffHref, reviewUrl } from "../../lib/codeUrl";
import {
  dispositionHint,
  findingDispositionChips,
  findingHeaderView,
  type FindingDisposition,
} from "../../lib/diffFindings";
import { relativeTime } from "../../lib/format";
import { questionStateForThread } from "../../lib/questionState";
import type { DiffCommentsApi } from "../../lib/reviewComments";
import {
  formatApplyConflictHint,
  splitSuggestionLines,
  suggestionIsOutdated,
  synthesizeSuggestionDiff,
  threadAcceptsSuggestion,
} from "../../lib/suggestions";
import { toast } from "../../lib/toast";
import SuggestionEditor from "./SuggestionEditor";
import UnifiedHunks from "./UnifiedHunks";
import SafeMarkdown from "../SafeMarkdown";
import HighlightedSnippet from "../HighlightedSnippet";
import { useHighlight } from "../../hooks/useHighlight";
import { offsetHighlightSpans, padSnippetLines, wireSpansToLineMap } from "../../lib/paintSpans";
import type { DiffHighlights } from "../../lib/diffHighlight";

/// Session-wide latch: one 404 from apply hides the button on every
/// thread for the rest of this SPA session (VerdictBar / StartReviewDialog
/// loopback-degrade, lifted to module scope so a remount doesn't retry).
let applyLoopbackLatched = false;
const applyLoopbackListeners = new Set<() => void>();

function latchApplyLoopback() {
  if (applyLoopbackLatched) return;
  applyLoopbackLatched = true;
  for (const l of applyLoopbackListeners) l();
}

function useApplyLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      applyLoopbackListeners.add(cb);
      return () => applyLoopbackListeners.delete(cb);
    },
    () => applyLoopbackLatched,
    () => applyLoopbackLatched,
  );
}

/// PRR-U3 — SEPARATE module-scoped latch for the disposition PUT/DELETE
/// (same "one 404 hides the button on every thread for the session" idiom
/// as `applyLoopbackLatched` above, kept as its own latch since apply and
/// disposition are independent action families — a non-loopback session
/// might still be mid-way through discovering which loopback-only actions
/// are unavailable).
let dispositionLoopbackLatched = false;
const dispositionLoopbackListeners = new Set<() => void>();

function latchDispositionLoopback() {
  if (dispositionLoopbackLatched) return;
  dispositionLoopbackLatched = true;
  for (const l of dispositionLoopbackListeners) l();
}

function useDispositionLoopbackLatched(): boolean {
  return useSyncExternalStore(
    (cb) => {
      dispositionLoopbackListeners.add(cb);
      return () => dispositionLoopbackListeners.delete(cb);
    },
    () => dispositionLoopbackLatched,
    () => dispositionLoopbackLatched,
  );
}

export interface DiffThreadProps {
  thread: ReviewComment;
  comments: DiffCommentsApi;
  /// Orphaned foot: show the "was psN:L…" label.
  orphaned?: boolean;
  className?: string;
}

export default function DiffThread({ thread, comments, orphaned = false, className }: DiffThreadProps) {
  const { repo = "" } = useParams<{ repo: string }>();
  const loopback = useApplyLoopbackLatched();
  const dispositionLoopback = useDispositionLoopbackLatched();
  const finding = comments.findingsById.get(thread.id) ?? null;
  const forceOpen = comments.flashThreadId === thread.id;
  const [expanded, setExpanded] = useState(() => !thread.resolved || forceOpen);
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const canSuggest = threadAcceptsSuggestion(thread);

  useEffect(() => {
    if (forceOpen) setExpanded(true);
  }, [forceOpen]);

  useEffect(() => {
    if (thread.resolved && !forceOpen) setExpanded(false);
    if (!thread.resolved) setExpanded(true);
  }, [thread.resolved, forceOpen]);

  async function onReply() {
    const trimmed = reply.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      await comments.onReply(thread.id, trimmed);
      setReply("");
    } catch (e) {
      toast.err(`couldn't reply: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function onResolve() {
    try {
      await comments.onResolve(thread.id, !thread.resolved);
    } catch (e) {
      toast.err(`couldn't update thread: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function onDelete() {
    try {
      await comments.onDelete(thread.id);
    } catch (e) {
      toast.err(`couldn't delete thread: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /// PRR-U3 — `d`isposition chip toggle: clicking the ACTIVE chip clears
  /// it (undecided), any other chip sets it. Own loopback latch, same
  /// idiom as `SuggestionBlock`'s apply button below.
  async function onSetDisposition(next: FindingDisposition) {
    if (!finding) return;
    const active = finding.disposition?.state === next;
    try {
      if (active) await comments.onClearDisposition(finding.slug);
      else await comments.onSetDisposition(finding.slug, next);
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) {
        latchDispositionLoopback();
        return;
      }
      toast.err(`couldn't update disposition: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function onCopyFindingPermalink() {
    if (!finding) return;
    const href = reviewDiffHref(repo, comments.reviewId, finding.location.path, {
      finding: finding.slug,
    });
    const url = `${window.location.origin}${href}`;
    try {
      await navigator.clipboard.writeText(url);
      toast.ok("Finding permalink copied");
    } catch {
      toast.err("couldn't copy permalink");
    }
  }

  const orig = thread.resolution.original;
  const wasLabel =
    orphaned && orig ? `was ps${orig.ps}:L${orig.line}` : orphaned ? "orphaned" : null;
  const replyCount = thread.replies.length;
  const findingView = finding ? findingHeaderView(finding) : null;
  const questionState = questionStateForThread(thread, finding);
  const cls = [
    "kbc-rthread",
    thread.resolved ? "kbc-rthread--resolved" : "",
    orphaned ? "kbc-rthread--orphaned" : "",
    forceOpen ? "kbc-rdiff__flash" : "",
    finding ? "kbc-rthread--finding" : "",
    finding ? `kbc-rthread--sev-${findingView?.severity}` : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  if (thread.resolved && !expanded) {
    return (
      <div
        className={cls}
        data-kbc-review-thread={thread.id}
        data-kbc-review-thread-resolved="true"
      >
        <button
          type="button"
          className="kbc-rthread__badge"
          onClick={() => setExpanded(true)}
          data-kbc-review-thread-expand={thread.id}
        >
          resolved · {replyCount} {replyCount === 1 ? "reply" : "replies"}
        </button>
      </div>
    );
  }

  return (
    <div
      className={cls}
      data-kbc-review-thread={thread.id}
      data-kbc-review-thread-resolved={thread.resolved ? "true" : "false"}
    >
      <div className="kbc-rthread__head">
        {findingView ? (
          <>
            <span
              className={`kbc-rthread__sev-chip kbc-rthread__sev-chip--${findingView.severity}`}
              data-kbc-finding-severity={findingView.severity}
            >
              {findingView.severityLabel}
            </span>
            <span className="kbc-rthread__category">{findingView.category}</span>
            <button
              type="button"
              className="kbc-rthread__slug"
              onClick={() => void onCopyFindingPermalink()}
              title="Copy finding permalink"
              data-kbc-finding-slug={findingView.slug}
            >
              {findingView.slug}
            </button>
          </>
        ) : (
          <IntentChip intent={thread.intent} />
        )}
        {questionState && (
          <span
            className={`kbc-rthread__qstate kbc-rthread__qstate--${questionState}`}
            data-kbc-question-state={questionState}
            title={
              questionState === "awaiting-agent"
                ? "Waiting on the agent"
                : "Waiting on you — the agent replied"
            }
          >
            {questionState === "awaiting-agent" ? "❓ awaiting agent" : "❓ awaiting you"}
          </span>
        )}
        <span className="kbc-rthread__meta">
          {findingView?.agentMark && <Icon.Spark />}
          {thread.author} · {relativeTime(thread.created_at)}
        </span>
        {wasLabel && (
          <span className="kbc-rthread__was" data-kbc-review-orphan-was={thread.id}>
            <Icon.Unlink />
            {wasLabel}
          </span>
        )}
        {thread.resolved && (
          <button
            type="button"
            className="kbc-rthread__collapse"
            onClick={() => setExpanded(false)}
            data-kbc-review-thread-expand={thread.id}
          >
            collapse
          </button>
        )}
      </div>
      {finding ? (
        <div className="kbc-rthread__finding-body" data-kbc-finding-body={finding.slug}>
          <p className="kbc-rthread__finding-title">{finding.title}</p>
          <div className="kbc-rthread__body">
            <SafeMarkdown text={finding.rationale} fences fallbackLang={finding.evidence?.lang} />
          </div>
          {finding.evidence?.source && (
            <div className="kbc-rthread__finding-evidence" data-kbc-finding-evidence>
              <HighlightedSnippet
                text={finding.evidence.source}
                lang={finding.evidence.lang}
                path={finding.location.path}
              />
            </div>
          )}
          {finding.recommendation && (
            <p className="kbc-rthread__finding-recommendation">
              <span aria-hidden="true">→ </span>
              {finding.recommendation}
            </p>
          )}
        </div>
      ) : (
        <div className="kbc-rthread__body">
          <SafeMarkdown text={thread.body} fences fallbackLang={null} />
        </div>
      )}
      {thread.suggestion && !editing && (
        <SuggestionBlock
          thread={thread}
          comments={comments}
          loopback={loopback}
          onEdit={() => setEditing(true)}
        />
      )}
      {editing && repo && (
        <SuggestionEditor
          repo={repo}
          thread={thread}
          existingReplacement={thread.suggestion?.replacement}
          onSave={async (replacement) => {
            await comments.onSetSuggestion(thread.id, replacement);
            setEditing(false);
          }}
          onCancel={() => setEditing(false)}
        />
      )}
      {thread.replies.map((r) => (
        <div key={r.id} className="kbc-rthread__reply" data-kbc-review-thread-reply={r.id}>
          <p className="kbc-rthread__body">{r.body}</p>
          <span className="kbc-rthread__meta">
            {r.author} · {relativeTime(r.created_at)}
          </span>
        </div>
      ))}
      <div className="kbc-rthread__reply-row">
        <input
          type="text"
          className="kbc-rthread__reply-input"
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          placeholder="Reply…"
          aria-label="reply body"
          data-kbc-review-thread-reply-body={thread.id}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void onReply();
            }
          }}
        />
        <button
          type="button"
          onClick={() => void onReply()}
          disabled={!reply.trim() || busy}
          data-kbc-review-thread-reply-submit={thread.id}
        >
          Reply
        </button>
      </div>
      <div className="kbc-rthread__actions">
        {canSuggest && !thread.suggestion && !editing && (
          <button
            type="button"
            onClick={() => setEditing(true)}
            data-kbc-suggestion-open={thread.id}
          >
            <Icon.Spark /> Suggest change
          </button>
        )}
        <button type="button" onClick={() => void onResolve()} data-kbc-review-thread-resolve={thread.id}>
          {thread.resolved ? "Reopen" : "Resolve"}
        </button>
        <button type="button" onClick={() => void onDelete()} data-kbc-review-thread-delete={thread.id}>
          Delete
        </button>
      </div>
      {finding && (
        <div className="kbc-rthread__disposition" data-kbc-finding-disposition={finding.slug}>
          {dispositionLoopback ? (
            <span className="kbc-rthread__disposition-loopback" data-kbc-disposition-loopback>
              Disposition requires a loopback session
            </span>
          ) : (
            findingDispositionChips(finding).map((chip) => (
              <button
                key={chip.value}
                type="button"
                className={
                  "kbc-rthread__disposition-chip" + (chip.active ? " is-active" : "")
                }
                title={dispositionHint(chip.value)}
                aria-pressed={chip.active}
                onClick={() => void onSetDisposition(chip.value)}
                data-kbc-disposition-chip={chip.value}
                data-kbc-disposition-active={chip.active ? "true" : "false"}
              >
                {chip.label}
              </button>
            ))
          )}
          <a
            className="kbc-rthread__view-in-report"
            href={`${reviewUrl(repo, comments.reviewId)}?tab=report#${encodeURIComponent(finding.slug)}`}
            data-kbc-finding-view-in-report={finding.slug}
          >
            View in report
          </a>
        </div>
      )}
    </div>
  );
}

function SuggestionBlock({
  thread,
  comments,
  loopback,
  onEdit,
}: {
  thread: ReviewComment;
  comments: DiffCommentsApi;
  loopback: boolean;
  onEdit: () => void;
}) {
  const confirm = useConfirm();
  const suggestion = thread.suggestion;
  const orig = suggestion?.original ?? "";
  const repl = suggestion?.replacement ?? "";
  const start = thread.resolution.line ?? 1;
  const hlItems = useMemo(
    () =>
      suggestion
        ? [
            { id: "old", lang: null as string | null, text: orig, path: thread.path },
            { id: "new", lang: null as string | null, text: repl, path: thread.path },
          ]
        : [],
    [suggestion, orig, repl, thread.path],
  );
  const { byId } = useHighlight(hlItems);
  const highlights: DiffHighlights | null = useMemo(() => {
    if (!suggestion) return null;
    const old = byId.get("old");
    const neu = byId.get("new");
    if (!old && !neu) return null;
    const oldSpans = old
      ? wireSpansToLineMap(padSnippetLines(orig, start).join("\n"), offsetHighlightSpans(old.spans, start))
      : new Map();
    const newSpans = neu
      ? wireSpansToLineMap(padSnippetLines(repl, start).join("\n"), offsetHighlightSpans(neu.spans, start))
      : new Map();
    return {
      oldLineSpans: oldSpans,
      newLineSpans: newSpans,
      oldLines: padSnippetLines(orig, start),
      newLines: padSnippetLines(repl, start),
    };
  }, [suggestion, byId, orig, repl, start]);
  if (!suggestion) return null;

  const outdated = suggestionIsOutdated(thread);
  const applied = suggestion.applied;
  const applyDisabled = applied || outdated;
  const parsed = synthesizeSuggestionDiff(
    splitSuggestionLines(suggestion.original),
    suggestion.replacement,
    start,
  );

  async function onRemove() {
    const ok = await confirm({
      title: "Remove this suggestion?",
      body: "The suggested replacement will be discarded. The comment stays.",
      confirmLabel: "Remove",
    });
    if (!ok) return;
    try {
      await comments.onClearSuggestion(thread.id);
    } catch (e) {
      toast.err(`couldn't remove suggestion: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function onApply() {
    if (applyDisabled) return;
    const resolveRef = { current: false };
    const ok = await confirm({
      title: "Apply this suggestion to the working tree?",
      body: <ApplyConfirmBody resolveRef={resolveRef} />,
      confirmLabel: "Apply",
      danger: true,
    });
    if (!ok) return;
    try {
      const out = await comments.onApplySuggestion(thread.id, resolveRef.current);
      if (out.already_applied) {
        toast.ok("Already applied — working tree already matches");
      } else {
        toast.ok("Suggestion applied");
      }
    } catch (e) {
      if (e instanceof ApplyConflictError) {
        toast.err(formatApplyConflictHint(e));
        return;
      }
      if (e instanceof ApiError && e.status === 404) {
        latchApplyLoopback();
        return;
      }
      toast.err(`couldn't apply suggestion: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return (
    <div className="kbc-rthread__suggest" data-kbc-review-thread-suggestion>
      <div className="kbc-suggestion__chips">
        {applied && (
          <span className="kbc-rthread__applied" data-kbc-suggestion-applied>
            <Icon.Check />
            applied
          </span>
        )}
        {outdated && (
          <span
            className="kbc-rthread__outdated"
            data-kbc-suggestion-outdated
            title="This comment's anchor no longer resolves on the target patchset"
          >
            <Icon.Warn />
            outdated
          </span>
        )}
      </div>
      <div className="kbc-suggestion__preview kbc-diff" data-kbc-suggestion-preview>
        <UnifiedHunks path={thread.path} parsed={parsed} highlights={highlights} />
      </div>
      <div className="kbc-suggestion__actions">
        {threadAcceptsSuggestion(thread) && (
          <button type="button" onClick={onEdit} data-kbc-suggestion-edit={thread.id}>
            <Icon.Pen /> Edit
          </button>
        )}
        <button type="button" onClick={() => void onRemove()} data-kbc-suggestion-remove={thread.id}>
          Remove
        </button>
        {loopback ? (
          <span className="kbc-suggestion__loopback" data-kbc-suggestion-loopback>
            Apply requires a loopback session
          </span>
        ) : (
          <button
            type="button"
            onClick={() => void onApply()}
            disabled={applyDisabled}
            data-kbc-suggestion-apply={thread.id}
          >
            Apply
          </button>
        )}
      </div>
    </div>
  );
}

function ApplyConfirmBody({ resolveRef }: { resolveRef: { current: boolean } }) {
  const [also, setAlso] = useState(false);
  return (
    <>
      <p>Apply this suggestion to the working tree?</p>
      <label className="kbc-suggestion__resolve">
        <input
          type="checkbox"
          checked={also}
          onChange={(e) => {
            setAlso(e.target.checked);
            resolveRef.current = e.target.checked;
          }}
          data-kbc-suggestion-apply-resolve
        />
        Also resolve the thread
      </label>
    </>
  );
}
