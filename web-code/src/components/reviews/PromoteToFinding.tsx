// V80-M5 (D6) — "Promote to finding": a human's OWN review comment (M0/M2's
// bind surface — a top-level, review-bound `ReviewComment`) is a PEER of
// the agent's imported findings, not a second class. This is the ONE
// component both call sites (`ReviewThreadsCard`'s Room sections,
// `ReviewFileThreadsPanel`'s reader rail) render on a bound human thread —
// see `review_findings.rs`'s "Adoption" doc for the server-side contract
// this mirrors: `category`/`title`/`rationale`/`location` all derive from
// the comment when the caller (this form) omits them, so the form only
// asks for what a promotion actually decides: severity, act, blocking, and
// an editable title.
import { useState } from "react";
import type { ReviewComment } from "../../api/types";
import { useCreateManualFindingMutation } from "../../hooks/useReviewComments";
import {
  REVIEW_MUTATIONS_ADMITTED_HINT,
  useReviewMutationsAdmitted,
} from "../../hooks/useReviewMutationsAdmitted";
import { SEVERITIES, severityLabel, type FindingSeverity } from "../../lib/diffFindings";
import { ACT_CHIPS } from "../../lib/reviewRoom";
import { toast } from "../../lib/toast";

const ACT_OPTIONS = Object.keys(ACT_CHIPS);

/// `review_findings.rs`'s `title_from_comment_body` mirrored client-side
/// ONLY for the form's prefill — the server computes its own default
/// independently (this is a convenience default, never load-bearing; a
/// caller's edited title, or an empty one, is what's actually sent).
function defaultTitle(body: string): string {
  const firstLine = (body.split("\n")[0] ?? "").trim();
  return firstLine.length > 80 ? firstLine.slice(0, 80) : firstLine;
}

/// A path-less, review-level "General" comment (`anchor_kind: "review"`,
/// `path === ""`) has no line for a finding to anchor against — the SAME
/// signal `ReviewThreadsCard`'s own General-section grouping uses. Refusing
/// PROACTIVELY here (a caption, never a submit that's guaranteed to 400) is
/// the same "tell the human before a round trip" posture V80-F2's loopback
/// pre-probe established.
function generalNoLocation(thread: ReviewComment): boolean {
  return thread.path === "";
}

export interface PromoteToFindingProps {
  repo: string;
  reviewId: number;
  thread: ReviewComment;
}

export default function PromoteToFinding({ repo, reviewId, thread }: PromoteToFindingProps) {
  const [open, setOpen] = useState(false);
  const [severity, setSeverity] = useState<FindingSeverity>("concern");
  const [act, setAct] = useState("issue");
  const [blocking, setBlocking] = useState(false);
  const [title, setTitle] = useState(() => defaultTitle(thread.body));
  const [busy, setBusy] = useState(false);
  const admitted = useReviewMutationsAdmitted();
  const noLocation = generalNoLocation(thread);
  const promote = useCreateManualFindingMutation(repo, reviewId);
  const disabled = !admitted || noLocation;
  const hint = noLocation
    ? "general comments have no file location — only line/range comments can become findings"
    : !admitted
      ? REVIEW_MUTATIONS_ADMITTED_HINT
      : undefined;

  if (!open) {
    return (
      <button
        type="button"
        className="kbc-promote-finding__toggle"
        disabled={disabled}
        title={hint}
        onClick={(e) => {
          // A sibling of `threadHref`'s own `<Link>` (never nested inside
          // it — HTML forbids nested interactive elements, the same reason
          // "open in reader" is a sibling too), but defensive stopPropagation
          // in case a future caller wraps this differently.
          e.preventDefault();
          e.stopPropagation();
          setTitle(defaultTitle(thread.body));
          setOpen(true);
        }}
        data-kbc-promote-finding-toggle={thread.id}
      >
        Promote to finding
      </button>
    );
  }

  async function submit() {
    if (disabled || busy) return;
    setBusy(true);
    try {
      await promote.mutateAsync({
        from_annotation_id: thread.id,
        severity,
        act,
        blocking,
        title: title.trim() || undefined,
      });
      setOpen(false);
    } catch (e) {
      // Root CLAUDE.md invariant #32 — a user-action `.catch` must
      // `toast.err`, never swallow.
      toast.err(`couldn't promote to finding: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="kbc-promote-finding"
      data-kbc-promote-finding-form={thread.id}
      onClick={(e) => e.preventDefault()}
    >
      {hint && (
        <p className="kbc-promote-finding__hint" data-kbc-promote-finding-hint title={hint}>
          {hint}
        </p>
      )}
      <div className="kbc-promote-finding__row">
        <label>
          <span>Severity</span>
          <select
            value={severity}
            onChange={(e) => setSeverity(e.target.value as FindingSeverity)}
            aria-label="finding severity"
            data-kbc-promote-finding-severity
          >
            {SEVERITIES.map((s) => (
              <option key={s} value={s}>
                {severityLabel(s)}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Act</span>
          <select
            value={act}
            onChange={(e) => setAct(e.target.value)}
            aria-label="finding act"
            data-kbc-promote-finding-act
          >
            {ACT_OPTIONS.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </select>
        </label>
        <label className="kbc-promote-finding__blocking">
          <input
            type="checkbox"
            checked={blocking}
            onChange={(e) => setBlocking(e.target.checked)}
            data-kbc-promote-finding-blocking
          />
          <span>blocking</span>
        </label>
      </div>
      <input
        type="text"
        className="kbc-promote-finding__title"
        value={title}
        onChange={(e) => setTitle(e.target.value)}
        placeholder="Title"
        aria-label="finding title"
        data-kbc-promote-finding-title
      />
      <div className="kbc-promote-finding__actions">
        <button
          type="button"
          onClick={() => void submit()}
          disabled={disabled || busy}
          title={hint}
          data-kbc-promote-finding-submit={thread.id}
        >
          {busy ? "Promoting…" : "Promote"}
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          data-kbc-promote-finding-cancel={thread.id}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}
