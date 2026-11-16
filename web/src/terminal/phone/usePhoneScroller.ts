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
  lockAxis,
  momentumStep,
  pinchFont,
  releaseVelocity,
  trimSamples,
  type Axis,
  type VelocitySample,
} from "./scrollerCore";
import { writeFont } from "./fontSteps";

/** The slice of xterm's Terminal the scroller touches (a fake in tests). */
export interface ScrollTerm {
  readonly rows: number;
  readonly element?: HTMLElement | undefined;
  readonly options: { fontSize?: number };
  readonly buffer: { readonly active: { readonly viewportY: number } };
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
}

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
  axis: Axis | null;
  samples: VelocitySample[];
}

export function usePhoneScroller(args: PhoneScrollerArgs): void {
  const { term, wrap, viewport } = args;
  const longPressRef = useRef(args.onLongPress);
  const activityRef = useRef(args.onActivity);
  const fontRef = useRef(args.onFont);
  fontRef.current = args.onFont;
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
      stopMomentum();
      clearPress();
      if (ptrs.size === 1) {
        drag = {
          x0: e.clientX,
          y0: e.clientY,
          lx: e.clientX,
          ly: e.clientY,
          axis: null,
          samples: [{ t: performance.now(), y: e.clientY }],
        };
        acc = 0;
        pressTimer = setTimeout(() => {
          pressTimer = null;
          if (drag && drag.axis === null && ptrs.size === 1) longPressRef.current();
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
        if (d.axis === "v") {
          const r = releaseVelocity(d.samples, performance.now());
          if (r.fling) startMomentum(r.v);
        }
      }
    };

    // xterm 6.0.0 has its own touch path; a real touch must not scroll twice.
    const swallow = (e: Event): void => {
      e.stopPropagation();
      if (e.cancelable) e.preventDefault();
    };

    viewport.addEventListener("pointerdown", onDown, true);
    viewport.addEventListener("pointermove", onMove, true);
    viewport.addEventListener("pointerup", onUp, true);
    viewport.addEventListener("pointercancel", onUp, true);
    const touchOpts = { capture: true, passive: false } as const;
    for (const t of ["touchstart", "touchmove", "touchend"]) {
      viewport.addEventListener(t, swallow, touchOpts);
    }
    return () => {
      viewport.removeEventListener("pointerdown", onDown, true);
      viewport.removeEventListener("pointermove", onMove, true);
      viewport.removeEventListener("pointerup", onUp, true);
      viewport.removeEventListener("pointercancel", onUp, true);
      for (const t of ["touchstart", "touchmove", "touchend"]) {
        viewport.removeEventListener(t, swallow, touchOpts);
      }
      stopMomentum();
      clearPress();
    };
  }, [term, wrap, viewport]);
}
