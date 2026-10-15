import {
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { detentHeight, next, type Detent, type DetentEvent } from "./sheet/detents";
import "./sheet.css";

export type { Detent } from "./sheet/detents";

export type SheetProps = {
  open: boolean;
  detent: Detent;
  onDetentChange: (d: Detent) => void;
  onClose: () => void;
  title: string;
  children: ReactNode;
  /** CSS height of the peek detent: px or vh. */
  peek?: string;
  /** CSS height of the full detent: px or vh. */
  full?: string;
  /** false: render children in a plain div, no dialog semantics. */
  asSheet?: boolean;
  id?: string;
};

function toPx(v: string): number {
  const n = parseFloat(v);
  if (!Number.isFinite(n)) return 0;
  if (v.trim().endsWith("vh")) {
    const h = typeof window === "undefined" ? 800 : window.innerHeight || 800;
    return (n / 100) * h;
  }
  return n;
}

const FOCUSABLE =
  'a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';

export function Sheet({
  open,
  detent,
  onDetentChange,
  onClose,
  title,
  children,
  peek = "40vh",
  full = "92vh",
  asSheet = true,
  id,
}: SheetProps) {
  const rid = useId();
  const titleId = `${id ?? "kb-sheet"}-title-${rid}`;
  const panelRef = useRef<HTMLDivElement>(null);
  const [dragOffset, setDragOffset] = useState<number | null>(null);
  const drag = useRef<{ y0: number; lastY: number; lastT: number; v: number } | null>(null);
  const active = asSheet && open;

  // Body scroll lock: restore the exact previous inline value.
  useEffect(() => {
    if (!active) return;
    const prev = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.body.style.overflow = prev;
    };
  }, [active]);

  // Focus into the sheet on open, back to the opener on close.
  useEffect(() => {
    if (!active) return;
    const opener = document.activeElement as HTMLElement | null;
    panelRef.current?.focus();
    return () => {
      opener?.focus?.();
    };
  }, [active]);

  if (!asSheet) return <div>{children}</div>;
  if (!open) return null;

  const peekPx = toPx(peek);
  const fullPx = toPx(full);
  const ev = (e: Partial<DetentEvent> & Pick<DetentEvent, "type">): DetentEvent => ({
    peekPx,
    fullPx,
    ...e,
  });
  const apply = (n: Detent) => {
    if (n === "closed") onClose();
    else if (n !== detent) onDetentChange(n);
  };

  const onKeyDown = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
      return;
    }
    if (e.key === "Tab") {
      const els = panelRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE);
      if (!els || els.length === 0) {
        e.preventDefault();
        return;
      }
      const first = els[0];
      const last = els[els.length - 1];
      const cur = document.activeElement;
      if (e.shiftKey && (cur === first || cur === panelRef.current)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && cur === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  const onHandleKeyDown = (e: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (e.key === "ArrowUp") {
      e.preventDefault();
      apply(next(detent, ev({ type: "up" })));
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      apply(next(detent, ev({ type: "down" })));
    }
  };

  const onPointerDown = (e: ReactPointerEvent<HTMLButtonElement>) => {
    e.currentTarget.setPointerCapture?.(e.pointerId);
    drag.current = { y0: e.clientY, lastY: e.clientY, lastT: e.timeStamp, v: 0 };
    setDragOffset(0);
  };
  const onPointerMove = (e: ReactPointerEvent<HTMLButtonElement>) => {
    const d = drag.current;
    if (!d) return;
    const dt = e.timeStamp - d.lastT;
    if (dt > 0) d.v = (e.clientY - d.lastY) / dt;
    d.lastY = e.clientY;
    d.lastT = e.timeStamp;
    setDragOffset(e.clientY - d.y0);
  };
  const endDrag = (e: ReactPointerEvent<HTMLButtonElement>) => {
    const d = drag.current;
    if (!d) return;
    drag.current = null;
    e.currentTarget.releasePointerCapture?.(e.pointerId);
    const offset = e.clientY - d.y0;
    setDragOffset(null);
    apply(next(detent, ev({ type: "dragEnd", offset, velocity: d.v })));
  };

  const base = detentHeight(detent, peekPx, fullPx);
  const h =
    dragOffset === null ? null : Math.max(0, Math.min(fullPx, base - dragOffset));
  const sheetH = h === null ? (detent === "full" ? full : peek) : `${h}px`;

  return (
    <>
      <div data-kb-scrim className="kb-sheet-scrim" onClick={onClose} />
      <div
        ref={panelRef}
        id={id}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        data-kb-sheet
        data-detent={detent}
        data-dragging={dragOffset === null ? undefined : ""}
        className="kb-sheet"
        tabIndex={-1}
        style={{ ["--sheet-h" as string]: sheetH }}
        onKeyDown={onKeyDown}
      >
        <button
          type="button"
          className="kb-sheet__handle"
          aria-label="Resize"
          onKeyDown={onHandleKeyDown}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={endDrag}
          onPointerCancel={endDrag}
        />
        <h2 id={titleId} className="kb-sheet__title">
          {title}
        </h2>
        <div className="kb-sheet__body">{children}</div>
      </div>
    </>
  );
}
