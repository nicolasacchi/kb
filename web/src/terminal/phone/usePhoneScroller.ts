// T17 — DOM wiring for the phone Watch view (port of the phone spike's
// pointer section). xterm 6.0.0 ships no touch handling, so one finger
// scrolls the buffer through `term.scrollLines`, a horizontal drag pans
// the wrapper (the grid is the real session grid, wider than the phone)
// and two fingers pinch through discrete font steps. cols/rows never
// change and nothing here uses a CSS transform (xterm.js issue 6023).

import { useEffect, useRef } from "react";
import {
  LONG_PRESS_MS,
  STOP_V,
  accumulateLines,
  cellAt,
  isStillTap,
  lockAxis,
  momentumStep,
  pinchFont,
  releaseVelocity,
  trimSamples,
  type Axis,
  type VelocitySample,
} from "./scrollerCore";
import { writeFont } from "./fontSteps";
import { linkAtCell, linkSpansRows, openExternal, type LinkRange, type RowSource } from "../links";
import { setPressedLine } from "./pressedLine";

/** The slice of xterm's Terminal the scroller touches (a fake in tests). */
export interface ScrollTerm {
  readonly rows: number;
  /** Needed for tap-to-open links (absent in a fake that has no grid). */
  readonly cols?: number | undefined;
  readonly element?: HTMLElement | undefined;
  readonly options: { fontSize?: number };
  readonly buffer: {
    readonly active: {
      readonly viewportY: number;
      readonly length?: number;
      getLine?(y: number): { translateToString(trimRight?: boolean): string; readonly isWrapped: boolean } | undefined;
    };
    readonly normal?: unknown;
  };
  scrollLines(n: number): void;
}

export interface PhoneScrollerArgs {
  term: ScrollTerm | null;
  wrap: HTMLElement | null;
  viewport: HTMLElement | null;
  onLongPress: () => void;
  /** Called after any scroll, pan or font change (attribute refresh). */
  onActivity?: () => void;
  /** Called with the new size when a pinch changes the font step. */
  onFont?: (px: number) => void;
  /**
   * PHIN0 — a still tap that is not on a URL asks for the input to be focused.
   * Absent = today's behaviour, byte for byte (touch events are fully swallowed).
   * Present = the touch handler no longer cancels the touch sequence, so the
   * browser still synthesizes the `click` the soft keyboard needs.
   */
  onTapFocus?: () => void;
  /** Which event calls `onTapFocus`: the synthesized `click` (default) or the pointerup itself. */
  focusOn?: "click" | "pointerup";
}

/** A still tap arms the click for this long. */
export const TAP_FOCUS_WINDOW_MS = 400;

/** `.xterm-screen` height over rows, 0 when not measurable. */
export function measureCellHeight(term: ScrollTerm): number {
  const screen = term.element?.querySelector(".xterm-screen");
  if (!screen || term.rows <= 0) return 0;
  return screen.getBoundingClientRect().height / term.rows;
}

/** Wrapper height follows the rendered screen (the grid never changes). */
export function relayoutWrap(term: ScrollTerm, wrap: HTMLElement): void {
  const screen = term.element?.querySelector(".xterm-screen");
  if (!screen) return;
  const h = Math.ceil(screen.getBoundingClientRect().height);
  if (h > 0) wrap.style.height = `${h}px`;
}

/** Set a font step: assign only on change, re-measure now and next frame, persist. */
export function applyFont(term: ScrollTerm, wrap: HTMLElement | null, px: number): boolean {
  if (term.options.fontSize === px) return false;
  (term.options as { fontSize?: number }).fontSize = px;
  writeFont(px);
  if (wrap) {
    relayoutWrap(term, wrap);
    if (typeof requestAnimationFrame === "function") {
      requestAnimationFrame(() => relayoutWrap(term, wrap));
    }
  }
  return true;
}

interface Drag {
  x0: number;
  y0: number;
  lx: number;
  ly: number;
  t0: number;
  /** The long press fired, or this touch stopped a coasting fling: the release is not a tap. */
  noTap: boolean;
  axis: Axis | null;
  samples: VelocitySample[];
}

/**
 * The link under a screen point, from the live buffer: the point maps to a cell
 * through the rendered `.xterm-screen`, the cell to a buffer line, and the line
 * (with its soft-wrapped neighbours) to a URL. Null off any URL.
 */
export function linkAtPoint(term: ScrollTerm, x: number, y: number): LinkRange | null {
  const cols = term.cols ?? 0;
  const buf = term.buffer.active;
  const screen = term.element?.querySelector(".xterm-screen");
  if (!screen || cols <= 0 || !buf.getLine) return null;
  const cell = cellAt(x, y, screen.getBoundingClientRect(), cols, term.rows);
  if (!cell) return null;
  const getLine = buf.getLine.bind(buf);
  const src: RowSource = {
    length: buf.length ?? buf.viewportY + term.rows,
    row: (i) => {
      const l = getLine(i);
      return l ? { text: l.translateToString(true), wrapped: l.isWrapped } : null;
    },
  };
  return linkAtCell(src, cols, cell.x, buf.viewportY + cell.row);
}

/** The inline confirm row for a URL that spans rows: the whole address, Open and Cancel (not window.confirm). */
function showLinkConfirm(host: HTMLElement, url: string, onOpen: () => void): () => void {
  const row = document.createElement("div");
  row.className = "kb-term-phone__linkrow";
  row.setAttribute("data-testid", "kb-term-link-confirm");
  const label = document.createElement("span");
  label.className = "kb-term-phone__linkurl";
  label.textContent = url;
  const mk = (text: string, id: string, fn: () => void): HTMLButtonElement => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "kb-term-phone__btn";
    b.setAttribute("data-testid", id);
    b.textContent = text;
    b.addEventListener("click", fn);
    return b;
  };
  const close = (): void => row.remove();
  row.append(
    label,
    mk("Open", "kb-term-link-open", () => {
      close();
      onOpen();
    }),
    mk("Cancel", "kb-term-link-cancel", close),
  );
  host.appendChild(row);
  return close;
}

/** The buffer line under a point, only on the normal screen (Read walks `buffer.normal`). */
function pressedBufferLine(term: ScrollTerm, x: number, y: number): number | null {
  const screen = term.element?.querySelector(".xterm-screen");
  const cols = term.cols ?? 0;
  if (!screen || cols <= 0 || term.buffer.normal !== term.buffer.active) return null;
  const cell = cellAt(x, y, screen.getBoundingClientRect(), cols, term.rows);
  return cell ? term.buffer.active.viewportY + cell.row : null;
}

export function usePhoneScroller(args: PhoneScrollerArgs): void {
  const { term, wrap, viewport } = args;
  const longPressRef = useRef(args.onLongPress);
  const activityRef = useRef(args.onActivity);
  const fontRef = useRef(args.onFont);
  fontRef.current = args.onFont;
  const tapFocusRef = useRef(args.onTapFocus);
  tapFocusRef.current = args.onTapFocus;
  const tapFocusOn = args.onTapFocus !== undefined;
  const focusOn = args.focusOn ?? "click";
  longPressRef.current = args.onLongPress;
  activityRef.current = args.onActivity;

  useEffect(() => {
    if (!term || !wrap || !viewport) return;
    const ptrs = new Map<number, { x: number; y: number }>();
    let drag: Drag | null = null;
    let pinch: { d0: number; startFont: number } | null = null;
    let acc = 0;
    let raf = 0;
    let pressTimer: ReturnType<typeof setTimeout> | null = null;
    let closeConfirm: (() => void) | null = null;
    /** When a still tap off any URL armed the click (null = not armed). */
    let tapArmedAt: number | null = null;
    const dismissConfirm = (): void => {
      closeConfirm?.();
      closeConfirm = null;
    };

    const clearPress = (): void => {
      if (pressTimer !== null) {
        clearTimeout(pressTimer);
        pressTimer = null;
      }
    };
    const stopMomentum = (): void => {
      if (raf) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
    };
    const activity = (): void => activityRef.current?.();

    /** Scroll by finger travel dy; false when the view did not move. */
    const applyDy = (dy: number): boolean => {
      const step = accumulateLines(acc, dy, measureCellHeight(term));
      acc = step.acc;
      if (step.lines === 0) return true;
      const before = term.buffer.active.viewportY;
      term.scrollLines(-step.lines);
      activity();
      return term.buffer.active.viewportY !== before;
    };

    const startMomentum = (v0: number): void => {
      let v = v0;
      let last = performance.now();
      const tick = (): void => {
        const now = performance.now();
        const dt = now - last;
        last = now;
        v = momentumStep(v, dt);
        const moved = applyDy(v * dt);
        raf = Math.abs(v) > STOP_V && moved ? requestAnimationFrame(tick) : 0;
      };
      raf = requestAnimationFrame(tick);
    };

    const dist = (): number => {
      const [a, b] = [...ptrs.values()];
      return a && b ? Math.hypot(a.x - b.x, a.y - b.y) : 0;
    };

    const onDown = (e: PointerEvent): void => {
      ptrs.set(e.pointerId, { x: e.clientX, y: e.clientY });
      try {
        viewport.setPointerCapture(e.pointerId);
      } catch {
        // synthetic pointers have no active pointer to capture
      }
      const coasting = raf !== 0;
      stopMomentum();
      clearPress();
      dismissConfirm();
      tapArmedAt = null;
      if (ptrs.size === 1) {
        drag = {
          x0: e.clientX,
          y0: e.clientY,
          lx: e.clientX,
          ly: e.clientY,
          t0: performance.now(),
          noTap: coasting,
          axis: null,
          samples: [{ t: performance.now(), y: e.clientY }],
        };
        acc = 0;
        pressTimer = setTimeout(() => {
          pressTimer = null;
          if (drag && drag.axis === null && ptrs.size === 1) {
            drag.noTap = true;
            // Read opens centred on this row (only meaningful on the normal screen).
            const hit = pressedBufferLine(term, drag.x0, drag.y0);
            setPressedLine(hit);
            longPressRef.current();
          }
        }, LONG_PRESS_MS);
      } else if (ptrs.size === 2) {
        drag = null;
        pinch = { d0: dist(), startFont: term.options.fontSize ?? 0 };
      }
    };

    const onMove = (e: PointerEvent): void => {
      if (!ptrs.has(e.pointerId)) return;
      ptrs.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (pinch && ptrs.size === 2) {
        if (pinch.d0 > 0) {
          const px = pinchFont(pinch.startFont, dist() / pinch.d0);
          if (applyFont(term, wrap, px)) {
            fontRef.current?.(px);
            activity();
          }
        }
        return;
      }
      if (!drag) return;
      const now = performance.now();
      if (drag.axis === null) {
        const axis = lockAxis(e.clientX - drag.x0, e.clientY - drag.y0);
        if (axis === null) return;
        drag.axis = axis;
        // Re-base to the START so the lock distance itself is scrolled too.
        drag.lx = drag.x0;
        drag.ly = drag.y0;
        clearPress();
      }
      if (drag.axis === "v") {
        applyDy(e.clientY - drag.ly);
        drag.samples.push({ t: now, y: e.clientY });
        trimSamples(drag.samples, now);
      } else {
        wrap.scrollLeft -= e.clientX - drag.lx;
        activity();
      }
      drag.lx = e.clientX;
      drag.ly = e.clientY;
    };

    const onUp = (e: PointerEvent): void => {
      if (!ptrs.has(e.pointerId)) return;
      ptrs.delete(e.pointerId);
      try {
        viewport.releasePointerCapture(e.pointerId);
      } catch {
        // already released
      }
      clearPress();
      if (pinch) {
        if (ptrs.size < 2) pinch = null;
        return;
      }
      if (drag && ptrs.size === 0) {
        const d = drag;
        drag = null;
        // A browser-cancelled gesture is not a tap and never flings.
        if (e.type === "pointercancel") return;
        if (!d.noTap && d.axis === null && isStillTap(e.clientX - d.x0, e.clientY - d.y0, performance.now() - d.t0)) {
          if (!tapAt(e.clientX, e.clientY) && tapFocusOn) {
            if (focusOn === "pointerup") tapFocusRef.current?.();
            else tapArmedAt = performance.now();
          }
          return;
        }
        if (d.axis === "v") {
          const r = releaseVelocity(d.samples, performance.now());
          if (r.fling) startMomentum(r.v);
        }
      }
    };

    /** A still tap: open the URL under it (a wrapped one is confirmed whole first). Never sent to the pty. */
    const tapAt = (x: number, y: number): boolean => {
      const link = linkAtPoint(term, x, y);
      if (!link) return false;
      const go = (): void => openExternal(link.url, window);
      if (!linkSpansRows(link)) {
        go();
        return true;
      }
      closeConfirm = showLinkConfirm(wrap.parentElement ?? wrap, link.url, go);
      return true;
    };

    // PHIN0: the click that follows a still tap raises the keyboard (Firefox on
    // Android only grants the IME from a click, not from pointerup).
    const onClick = (): void => {
      const at = tapArmedAt;
      tapArmedAt = null;
      if (at !== null && performance.now() - at <= TAP_FOCUS_WINDOW_MS) tapFocusRef.current?.();
    };

    // xterm 6.0.0 has its own touch path; a real touch must not scroll twice.
    // With tap-to-focus on, preventDefault is NOT called: it cancels the
    // synthesized click, and `touch-action:none` already stops browser scrolling.
    const swallow = (e: Event): void => {
      e.stopPropagation();
      if (!tapFocusOn && e.cancelable) e.preventDefault();
    };

    viewport.addEventListener("pointerdown", onDown, true);
    viewport.addEventListener("pointermove", onMove, true);
    viewport.addEventListener("pointerup", onUp, true);
    viewport.addEventListener("pointercancel", onUp, true);
    if (tapFocusOn) viewport.addEventListener("click", onClick, true);
    const touchOpts = { capture: true, passive: false } as const;
    for (const t of ["touchstart", "touchmove", "touchend"]) {
      viewport.addEventListener(t, swallow, touchOpts);
    }
    return () => {
      viewport.removeEventListener("pointerdown", onDown, true);
      viewport.removeEventListener("pointermove", onMove, true);
      viewport.removeEventListener("pointerup", onUp, true);
      viewport.removeEventListener("pointercancel", onUp, true);
      viewport.removeEventListener("click", onClick, true);
      for (const t of ["touchstart", "touchmove", "touchend"]) {
        viewport.removeEventListener(t, swallow, touchOpts);
      }
      stopMomentum();
      clearPress();
      dismissConfirm();
    };
  }, [term, wrap, viewport, tapFocusOn, focusOn]);
}
