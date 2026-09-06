import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import type { ResurfaceItem } from "../api/generated/ResurfaceItem";
import type { ResurfaceWeights } from "../api/generated/ResurfaceWeights";
import { useFocusTrap } from "../hooks/useFocusTrap";
import { artifactHref } from "../lib/artifactHref";
import { censusBump } from "../lib/census";
import { fmtScore, type ScoreTerm } from "./ScoreExplain";

// W2.9 — fallback ONLY for a daemon predating the `weights` wire field
// (mirrors kb_core::resurface::ResurfaceWeights::default()). Any daemon
// that actually sends `weights` always wins — this is never used to
// override a live value.
const DEFAULT_WEIGHTS: ResurfaceWeights = {
  comment_weight: 0.6,
  read_weight: 0.4,
  comment_saturation: 4,
  read_halflife_days: 45.0,
};

// Review mode — a pull SESSION over the resurface queue, one card at a
// time (never a persisted "unread" list). Acting-clears-by-consequence
// ONLY: there is no mark-done storage here. Opening an artifact records a
// history visit (detail.tsx's recordOpen on mount), and the server's
// queue naturally drops the item on the next fetch as its comment/read
// signals change — exactly like the strip itself. The only client state
// this component owns is "which card is showing right now."
//
// Shared with ResurfaceStrip's per-item scorechip so the arithmetic
// reads identically whether you're skimming the strip or working the
// queue card-by-card.
export function resurfaceScoreTerms(item: ResurfaceItem, weights?: ResurfaceWeights): ScoreTerm[] {
  const w = weights ?? DEFAULT_WEIGHTS;
  return [
    {
      label: "comment",
      value: item.comment_term,
      detail: `min(${item.open_comments}, ${w.comment_saturation})/${w.comment_saturation} × ${w.comment_weight}`,
      color: "var(--accent)",
    },
    {
      label: "read",
      value: item.read_term,
      detail: `${item.completion_pct ?? 0}% × 0.5^(idle/${w.read_halflife_days}d) × ${w.read_weight}`,
      color: "var(--blue)",
    },
  ];
}

export function ResurfaceReview({
  kb,
  items,
  weights,
  onClose,
}: {
  kb: string;
  items: ResurfaceItem[];
  weights?: ResurfaceWeights;
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const [idx, setIdx] = useState(0);
  const cardRef = useRef<HTMLDivElement | null>(null);
  useFocusTrap(cardRef, true);

  // "on open of the overlay" — fires once per mount, regardless of how the
  // caller opened it.
  useEffect(() => {
    censusBump("resurface.review");
  }, []);

  // Move focus into the dialog on mount (the container itself, not a
  // control) — otherwise focus stays on the "review N" trigger behind the
  // overlay and the first Enter keystroke would land there instead of
  // opening the current card.
  useEffect(() => {
    cardRef.current?.focus();
  }, []);

  // The queue can shrink out from under an open review (another tab
  // resolves a comment, a beacon lands) — if it empties entirely, there's
  // nothing left to review.
  useEffect(() => {
    if (items.length === 0) onClose();
  }, [items.length, onClose]);

  const clamped = items.length === 0 ? 0 : Math.min(idx, items.length - 1);
  const current = items[clamped] as ResurfaceItem | undefined;

  const advance = useCallback(() => {
    setIdx((i) => Math.min(i + 1, items.length - 1));
  }, [items.length]);
  const retreat = useCallback(() => {
    setIdx((i) => Math.max(i - 1, 0));
  }, []);

  const openCurrent = useCallback(
    (panel?: "comments") => {
      if (!current) return;
      censusBump("resurface.click");
      navigate(artifactHref(kb, current.source_relative, panel ? { panel } : undefined));
      onClose();
    },
    [current, kb, navigate, onClose],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) {
        return;
      }
      switch (e.key) {
        case "Escape":
          e.preventDefault();
          onClose();
          return;
        case "Enter":
          // A focused button/link already owns Enter (Next/Done/Open
          // comments) — only treat bare Enter as "open current" when focus
          // isn't already on one of the dialog's own controls, so tabbing
          // to "Next" and pressing Enter advances instead of also opening.
          if (t?.tagName === "BUTTON" || t?.tagName === "A") return;
          e.preventDefault();
          openCurrent();
          return;
        case "j":
        case "ArrowDown":
        case "ArrowRight":
          e.preventDefault();
          advance();
          return;
        case "k":
        case "ArrowUp":
        case "ArrowLeft":
          e.preventDefault();
          retreat();
          return;
        default:
          return;
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [advance, retreat, openCurrent, onClose]);

  if (!current) return null;
  const terms = resurfaceScoreTerms(current, weights);

  return (
    <div className="kb-rsv-scrim" role="presentation" onClick={onClose}>
      <div
        ref={cardRef}
        className="kb-rsv"
        role="dialog"
        aria-modal="true"
        aria-label={`Review: ${current.title}`}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="kb-rsv__pos">
          {clamped + 1} of {items.length}
        </div>
        <h2 className="kb-rsv__title">{current.title}</h2>
        {current.reasons.length > 0 && (
          <ul className="kb-rsv__reasons">
            {current.reasons.map((r) => (
              <li key={r} className="kb-rsv__reason">
                {r}
              </li>
            ))}
          </ul>
        )}
        <div className="kb-rsv__score">
          <div className="kb-rsv__score-head">
            <span className="kb-scx__title">resurface score</span>
            <span className="kb-scx__total">{fmtScore(current.score)}</span>
          </div>
          <ul className="kb-scx__rows">
            {terms.map((t) => (
              <li key={t.label} className="kb-scx__row">
                <i className="kb-scx__dot" style={{ background: t.color }} aria-hidden="true" />
                <span className="kb-scx__label">{t.label}</span>
                <span className="kb-scx__val">
                  {typeof t.value === "string" ? t.value : fmtScore(t.value)}
                </span>
                {t.detail && <span className="kb-scx__detail">{t.detail}</span>}
              </li>
            ))}
          </ul>
          <div className="kb-scx__foot">score = comment + read</div>
        </div>
        <div className="kb-rsv__meta">
          {current.est_min_left != null && (
            <span className="kb-rsv__meta-item">~{current.est_min_left} min left</span>
          )}
          <span className="kb-rsv__meta-item">
            {current.open_comments} open comment{current.open_comments === 1 ? "" : "s"}
          </span>
        </div>
        <div className="kb-rsv__actions">
          <button
            type="button"
            className="kb-rsv__btn kb-rsv__btn--primary"
            data-kb-act="resurface-review-open"
            onClick={() => openCurrent()}
          >
            Open
          </button>
          <button
            type="button"
            className="kb-rsv__btn"
            data-kb-act="resurface-review-open-comments"
            onClick={() => openCurrent("comments")}
          >
            Open comments
          </button>
          <button
            type="button"
            className="kb-rsv__btn"
            data-kb-act="resurface-review-next"
            disabled={clamped >= items.length - 1}
            onClick={advance}
          >
            Next
          </button>
          <button
            type="button"
            className="kb-rsv__btn"
            data-kb-act="resurface-review-done"
            onClick={onClose}
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
