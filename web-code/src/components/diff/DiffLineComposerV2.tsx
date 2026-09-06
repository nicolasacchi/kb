import { useState } from "react";
import type { AnnotationIntent } from "../../api/types";
import { INTENT_OPTIONS, intentLabel } from "../../lib/annotations";
import { SEVERITIES, severityLabel, type FindingSeverity } from "../../lib/diffFindings";
import type { DiffSide } from "../../lib/reviewComments";
import { toast } from "../../lib/toast";

/// PRR-U3 (addendum §E) — the composer's finding-mode draft. `path`/`side`/
/// `line` are already known to the caller (the composer is always opened at
/// a specific gutter line) — this carries only what the OPERATOR types.
export interface FindingDraftInput {
  severity: string;
  category: string;
  title: string;
  rationale: string;
  recommendation: string;
}

/// PRR-U4 (§4) adds `"question"` — a third peer beside comment/finding,
/// always offered (unlike finding mode, which needs `onSubmitFinding`):
/// posts through the SAME `onSubmit` every composer already has, hardcoding
/// `intent: "question"` so the resulting thread shows the ❓ awaiting-agent
/// chip (`lib/questionState.ts`) until a claude-authored reply lands. The
/// plain "comment" mode's own Intent select still offers `question` too
/// (unchanged, `lib/annotations.ts`'s `INTENT_OPTIONS`) — this mode is a
/// one-click shortcut for the common case, not a replacement for it.
export type ComposerMode = "comment" | "finding" | "question";

export interface DiffLineComposerV2Props {
  side: DiffSide;
  line: number;
  onSubmit: (body: string, intent: string) => Promise<void>;
  /// PRR-U3 (addendum §E) — creates a manual finding at this composer's
  /// line/side. Omitted callers (none in this crate today, but kept
  /// optional for forward-compat) never show the "finding" mode segment —
  /// a composer that can't create a finding shouldn't offer to.
  onSubmitFinding?: (draft: FindingDraftInput) => Promise<void>;
  onDone: () => void;
}

/// Review-scoped inline composer (both sides). Legacy `DiffLineComposer`
/// (Commit/Compare, `anchor_kind: "diff"`) is untouched.
///
/// PRR-U3 adds a `comment` / `finding` mode segment (addendum §E). Finding
/// mode swaps the intent select for severity + category + title/rationale/
/// recommendation fields and posts through `onSubmitFinding` instead of
/// `onSubmit` — findings are ALWAYS code-anchored at this composer's own
/// line (design-addendum-2 §E: "no path-less finding from here"), so there
/// is no path/location picker here at all, only the fields a human adds on
/// top of the anchor the caller already resolved.
export default function DiffLineComposerV2({
  side,
  line,
  onSubmit,
  onSubmitFinding,
  onDone,
}: DiffLineComposerV2Props) {
  const [mode, setMode] = useState<ComposerMode>("comment");
  const [body, setBody] = useState("");
  const [intent, setIntent] = useState<AnnotationIntent>("note");
  const [severity, setSeverity] = useState<FindingSeverity>("concern");
  const [category, setCategory] = useState("");
  const [title, setTitle] = useState("");
  const [rationale, setRationale] = useState("");
  const [recommendation, setRecommendation] = useState("");
  const [busy, setBusy] = useState(false);

  async function saveComment() {
    const trimmed = body.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      await onSubmit(trimmed, intent);
      onDone();
    } catch (e) {
      toast.err(`couldn't save comment: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  async function saveFinding() {
    if (busy || !onSubmitFinding) return;
    if (!category.trim() || !title.trim() || !rationale.trim()) return;
    setBusy(true);
    try {
      await onSubmitFinding({ severity, category, title, rationale, recommendation });
      onDone();
    } catch (e) {
      toast.err(`couldn't save finding: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  /// PRR-U4 — question mode shares the SAME `body` state as comment mode
  /// (both are plain-text composes to `onSubmit`); only the posted intent
  /// differs.
  async function saveQuestion() {
    const trimmed = body.trim();
    if (!trimmed || busy) return;
    setBusy(true);
    try {
      await onSubmit(trimmed, "question");
      onDone();
    } catch (e) {
      toast.err(`couldn't ask: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }

  const showFindingMode = !!onSubmitFinding;
  const canSaveFinding = !!category.trim() && !!title.trim() && !!rationale.trim() && !busy;
  const modeLabel = mode === "finding" ? "Finding" : mode === "question" ? "Question" : "Comment";

  return (
    <div className="kbc-rcompose" data-kbc-review-composer={`${side}:${line}`}>
      <div className="kbc-rcompose__label">
        {modeLabel} on {side} side — line {line}
      </div>
      <div
        className="kbc-rcompose__mode"
        role="group"
        aria-label="composer mode"
        data-kbc-review-composer-mode
      >
        <button
          type="button"
          className={"kbc-rcompose__mode-btn" + (mode === "comment" ? " is-active" : "")}
          aria-pressed={mode === "comment"}
          onClick={() => setMode("comment")}
          data-kbc-review-composer-mode-btn="comment"
        >
          Comment
        </button>
        {showFindingMode && (
          <button
            type="button"
            className={"kbc-rcompose__mode-btn" + (mode === "finding" ? " is-active" : "")}
            aria-pressed={mode === "finding"}
            onClick={() => setMode("finding")}
            data-kbc-review-composer-mode-btn="finding"
          >
            Finding
          </button>
        )}
        <button
          type="button"
          className={"kbc-rcompose__mode-btn" + (mode === "question" ? " is-active" : "")}
          aria-pressed={mode === "question"}
          onClick={() => setMode("question")}
          data-kbc-review-composer-mode-btn="question"
        >
          Question
        </button>
      </div>
      {mode === "question" ? (
        <>
          <p className="kbc-rcompose__hint" data-kbc-review-composer-question-hint>
            Posts as a question at line {line} — shows ❓ awaiting agent until a claude-authored reply
            lands.
          </p>
          <textarea
            className="kbc-rcompose__body"
            placeholder="What do you want to ask about this line?"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            rows={3}
            aria-label="question body"
            autoFocus
            data-kbc-review-composer-question-body
          />
          <div className="kbc-rcompose__row">
            <div className="kbc-rcompose__actions">
              <button
                type="button"
                onClick={() => void saveQuestion()}
                disabled={!body.trim() || busy}
                data-kbc-review-composer-submit-question
              >
                {busy ? "Asking…" : "Ask"}
              </button>
              <button type="button" onClick={onDone} data-kbc-review-composer-cancel>
                Cancel
              </button>
            </div>
          </div>
        </>
      ) : mode === "finding" && showFindingMode ? (
        <>
          <p className="kbc-rcompose__hint" data-kbc-review-composer-finding-hint>
            Findings are code-anchored — this creates one at line {line}. For a general point, use a
            comment instead.
          </p>
          <div className="kbc-rcompose__row">
            <label className="kbc-rcompose__intent">
              <span>Severity</span>
              <select
                value={severity}
                onChange={(e) => setSeverity(e.target.value as FindingSeverity)}
                aria-label="finding severity"
                data-kbc-review-composer-severity
              >
                {SEVERITIES.map((s) => (
                  <option key={s} value={s}>
                    {severityLabel(s)}
                  </option>
                ))}
              </select>
            </label>
            <input
              type="text"
              className="kbc-rcompose__category"
              placeholder="Category (e.g. correctness)"
              value={category}
              onChange={(e) => setCategory(e.target.value)}
              aria-label="finding category"
              data-kbc-review-composer-category
            />
          </div>
          <input
            type="text"
            className="kbc-rcompose__title"
            placeholder="Title"
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            aria-label="finding title"
            data-kbc-review-composer-finding-title
          />
          <textarea
            className="kbc-rcompose__body"
            placeholder="Rationale — why this matters"
            value={rationale}
            onChange={(e) => setRationale(e.target.value)}
            rows={3}
            aria-label="finding rationale"
            data-kbc-review-composer-rationale
          />
          <textarea
            className="kbc-rcompose__body"
            placeholder="Recommendation (optional)"
            value={recommendation}
            onChange={(e) => setRecommendation(e.target.value)}
            rows={2}
            aria-label="finding recommendation"
            data-kbc-review-composer-recommendation
          />
          <div className="kbc-rcompose__row">
            <div className="kbc-rcompose__actions">
              <button
                type="button"
                onClick={() => void saveFinding()}
                disabled={!canSaveFinding}
                data-kbc-review-composer-submit-finding
              >
                {busy ? "Saving…" : "Create finding"}
              </button>
              <button type="button" onClick={onDone} data-kbc-review-composer-cancel>
                Cancel
              </button>
            </div>
          </div>
        </>
      ) : (
        <>
          <textarea
            className="kbc-rcompose__body"
            placeholder="What's worth flagging about this line?"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            rows={3}
            aria-label="review comment body"
            autoFocus
            data-kbc-review-composer-body
          />
          <div className="kbc-rcompose__row">
            <label className="kbc-rcompose__intent">
              <span>Intent</span>
              <select
                value={intent}
                onChange={(e) => setIntent(e.target.value as AnnotationIntent)}
                aria-label="comment intent"
                data-kbc-review-composer-intent
              >
                {INTENT_OPTIONS.map((opt) => (
                  <option key={opt} value={opt}>
                    {intentLabel(opt)}
                  </option>
                ))}
              </select>
            </label>
            <div className="kbc-rcompose__actions">
              <button
                type="button"
                onClick={() => void saveComment()}
                disabled={!body.trim() || busy}
                data-kbc-review-composer-submit
              >
                {busy ? "Saving…" : "Submit"}
              </button>
              <button type="button" onClick={onDone} data-kbc-review-composer-cancel>
                Cancel
              </button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
