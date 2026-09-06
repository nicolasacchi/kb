/*
 * ScoreExplain — the scorechip popover (W1.chip).
 *
 * One shared primitive for every decomposed-score surface (resurface strip,
 * search cards, related-memories rows, atlas label terms): a trigger chip
 * that opens a small popover showing the exact arithmetic behind a score —
 * a stacked bar plus per-term rows. Displays server-provided terms only;
 * never re-scores client-side (the decomposition IS the wire contract).
 *
 * Progressive enhancement: native Popover API when available (top layer,
 * light dismiss via the toggle event); otherwise a fixed-position div at
 * --z-popover with outside-click + Esc dismiss. Both paths position from
 * the trigger rect, clamped to the viewport.
 */
import { useCallback, useEffect, useId, useRef, useState } from "react";

export type ScoreTerm = {
  label: string;
  /** numeric values render in the stacked bar; string values (e.g. a raw
   *  per-arm rank like "#4") render verbatim in the row and are excluded
   *  from the bar — the honest shape when a term isn't a score share. */
  value: number | string;
  /** the arithmetic behind the value, e.g. "min(3, 4)/4 × 0.6" */
  detail?: string;
  /** CSS color for the bar segment; defaults cycle through the token set */
  color?: string;
};

export type ScoreExplainProps = {
  /** heading + aria label, e.g. "resurface score" */
  label: string;
  total: number;
  terms: ScoreTerm[];
  /** identity line under the rows, e.g. "score = comment_term + read_term" */
  footnote?: string;
  format?: (n: number) => string;
  className?: string;
  /** render the trigger as a span[role=button] instead of a real <button> —
   *  required when the chip sits INSIDE an anchor (search cards are one big
   *  <Link>; button-in-anchor is invalid HTML). Keyboard: Enter/Space. */
  inline?: boolean;
  children: React.ReactNode;
};

const SEG_COLORS = ["var(--accent)", "var(--blue)", "var(--green)", "var(--warn)"];
const POP_WIDTH = 264;

export function fmtScore(n: number): string {
  if (!Number.isFinite(n)) return "—";
  return Math.abs(n) >= 100 ? String(Math.round(n)) : n.toFixed(3).replace(/\.?0+$/, "") || "0";
}

/** Clamp the popover's left edge into the viewport with an 8px gutter. */
export function clampLeft(triggerLeft: number, viewportW: number, popW = POP_WIDTH): number {
  return Math.max(8, Math.min(triggerLeft, viewportW - popW - 8));
}

const supportsPopover =
  typeof HTMLElement !== "undefined" && "showPopover" in HTMLElement.prototype;

export function ScoreExplain({
  label,
  total,
  terms,
  footnote,
  format = fmtScore,
  className,
  inline = false,
  children,
}: ScoreExplainProps) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);
  const btnRef = useRef<HTMLButtonElement | null>(null);
  const popRef = useRef<HTMLDivElement | null>(null);
  const id = useId();

  const place = useCallback(() => {
    const r = btnRef.current?.getBoundingClientRect();
    if (!r) return;
    setPos({ top: r.bottom + 6, left: clampLeft(r.left, window.innerWidth) });
  }, []);

  const toggle = useCallback(
    (e: React.MouseEvent) => {
      // score chips often live inside <Link> cards — never navigate
      e.preventDefault();
      e.stopPropagation();
      place();
      setOpen((o) => !o);
    },
    [place],
  );

  // Native popover path ("manual" so we own dismissal on both paths):
  // show/hide imperatively for the top-layer benefit.
  useEffect(() => {
    const el = popRef.current;
    if (!el || !supportsPopover) return;
    try {
      if (open && !el.matches(":popover-open")) el.showPopover();
      else if (!open && el.matches(":popover-open")) el.hidePopover();
    } catch {
      /* detached mid-transition — harmless */
    }
  }, [open]);

  // Dismissal (both paths): outside click + Esc.
  useEffect(() => {
    if (!open) return;
    const onDown = (ev: MouseEvent) => {
      const t = ev.target as Node;
      if (popRef.current?.contains(t) || btnRef.current?.contains(t)) return;
      setOpen(false);
    };
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const shown = terms.filter(
    (t) => typeof t.value === "string" || Number.isFinite(t.value),
  );
  // Only numeric terms are bar segments; string terms (raw ranks etc.)
  // are row-only.
  const barTerms = shown.filter(
    (t): t is ScoreTerm & { value: number } => typeof t.value === "number",
  );
  const sum = barTerms.reduce((a, t) => a + Math.max(0, t.value), 0);

  const triggerProps = {
    className: "kb-scx__chip",
    "aria-expanded": open,
    "aria-controls": id,
    "aria-label": `explain ${label}`,
    onClick: toggle,
  };

  return (
    <span className={`kb-scx${className ? ` ${className}` : ""}`}>
      {inline ? (
        <span
          ref={btnRef as unknown as React.Ref<HTMLSpanElement>}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              toggle(e as unknown as React.MouseEvent);
            }
          }}
          {...triggerProps}
        >
          {children}
        </span>
      ) : (
        <button ref={btnRef} type="button" {...triggerProps}>
          {children}
        </button>
      )}
      <div
        ref={popRef}
        id={id}
        role="dialog"
        aria-label={`${label} decomposition`}
        className="kb-scx__pop"
        {...(supportsPopover ? { popover: "manual" as const } : {})}
        style={
          pos
            ? { position: "fixed", top: pos.top, left: pos.left, width: POP_WIDTH }
            : { display: "none" }
        }
        hidden={!open}
      >
        <div className="kb-scx__head">
          <span className="kb-scx__title">{label}</span>
          <span className="kb-scx__total">{format(total)}</span>
        </div>
        {sum > 0 && (
          <div className="kb-scx__bar" aria-hidden="true">
            {barTerms.map((t, i) => (
              <i
                key={t.label}
                className="kb-scx__seg"
                style={{
                  width: `${(Math.max(0, t.value) / sum) * 100}%`,
                  background: t.color ?? SEG_COLORS[i % SEG_COLORS.length],
                }}
              />
            ))}
          </div>
        )}
        <ul className="kb-scx__rows">
          {shown.map((t, i) => (
            <li key={t.label} className="kb-scx__row">
              <i
                className="kb-scx__dot"
                style={{ background: t.color ?? SEG_COLORS[i % SEG_COLORS.length] }}
                aria-hidden="true"
              />
              <span className="kb-scx__label">{t.label}</span>
              <span className="kb-scx__val">
                {typeof t.value === "string" ? t.value : format(t.value)}
              </span>
              {t.detail && <span className="kb-scx__detail">{t.detail}</span>}
            </li>
          ))}
        </ul>
        {footnote && <div className="kb-scx__foot">{footnote}</div>}
      </div>
    </span>
  );
}
