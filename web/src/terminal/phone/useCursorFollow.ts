// PHIN1 - keeps the terminal cursor cell in view above the soft keyboard while
// the operator types directly (the hidden direct-typing pref). Triggers are the
// terminal's own parse/cursor events, the proxy gaining focus (this hook being
// enabled) and the visual viewport's resize and scroll; every one of them goes
// through ONE rAF-coalesced `schedule`, and a frame reads first, then writes once.
//
// Horizontal follow is `wrap.scrollLeft`. Vertical follow cannot use a scroll
// position: the Watch box is bottom-anchored with `overflow:hidden` and the pan
// wrapper is as tall as the whole grid, so rows clipped at the top are not
// reachable by scrolling. The wrapper's `margin-bottom` moves the grid instead
// (positive lifts it, negative lowers it), with no CSS transform (xterm issue
// 6023) and never a grid resize.

import { useEffect } from "react";
import { planFollow, visibleBand, type FollowGate, type Rect, type VV } from "./cursorFollow";

/** The slice of xterm the follow reads (a fake in tests). */
export interface FollowTerm {
  readonly cols: number;
  readonly rows: number;
  readonly element?: HTMLElement | undefined;
  readonly buffer: {
    readonly active: { readonly cursorX: number; readonly cursorY: number; readonly viewportY: number; readonly baseY: number };
  };
  scrollToBottom(): void;
  onWriteParsed?(cb: () => void): { dispose(): void };
  onCursorMove?(cb: () => void): { dispose(): void };
}

export interface CursorFollowArgs {
  term: FollowTerm | null;
  /** The horizontal pan wrapper. */
  wrap: HTMLElement | null;
  /** The bottom-anchored Watch box: what the operator sees of the grid. */
  watch: HTMLElement | null;
  enabled: boolean;
  gate: FollowGate;
}

export function useCursorFollow(args: CursorFollowArgs): void {
  const { term, wrap, watch, enabled, gate } = args;

  useEffect(() => {
    if (!enabled || !term || !wrap || !watch) return;
    const vv = typeof window === "undefined" ? null : window.visualViewport;
    let raf = 0;
    /** One-shot re-arm for a frame the suspend gate dropped (nothing else may fire before the operator looks). */
    let rearm: ReturnType<typeof setTimeout> | null = null;
    /** The vertical offset this hook wrote to the wrapper's margin-bottom (px, positive lifts the grid). */
    let off = 0;

    const run = (): void => {
      raf = 0;
      if (!gate.allowed()) {
        if (rearm === null) {
          rearm = setTimeout(() => {
            rearm = null;
            schedule();
          }, Math.max(1, Math.ceil(gate.remainingMs())));
        }
        return;
      }
      const b = term.buffer.active;
      if (b.viewportY < b.baseY) term.scrollToBottom();

      // Reads.
      const screen = term.element?.querySelector(".xterm-screen");
      if (!screen || term.cols <= 0 || term.rows <= 0) return;
      const s = screen.getBoundingClientRect();
      const w = watch.getBoundingClientRect();
      const wrapH = wrap.getBoundingClientRect().height;
      const view: VV | null = vv
        ? { offsetTop: vv.offsetTop, offsetLeft: vv.offsetLeft, width: vv.width, height: vv.height }
        : null;
      const cw = s.width / term.cols;
      const ch = s.height / term.rows;
      const left = s.left + b.cursorX * cw;
      const top = s.top + b.cursorY * ch;
      const cursor: Rect = { left, top, right: left + cw, bottom: top + ch };
      const band = visibleBand(w, view);
      if (!(band.bottom - band.top > 0) || !(band.right - band.left > 0)) return;
      const plan = planFollow({ cursor, band, cellW: cw, cellH: ch });
      let next = off;
      if (plan.dy !== 0) {
        next = off + plan.dy;
      } else if (off !== 0) {
        // Back to rest once the cursor would be inside the band without our offset.
        const rest = planFollow({ cursor: { ...cursor, top: cursor.top + off, bottom: cursor.bottom + off }, band, cellW: cw, cellH: ch });
        if (rest.dy === 0) next = 0;
      }
      const lim = Math.max(0, wrapH - ch);
      next = Math.round(Math.max(-lim, Math.min(lim, next)));

      // One write.
      if (plan.dx !== 0) wrap.scrollLeft += plan.dx;
      if (next !== off) {
        off = next;
        wrap.style.marginBottom = next === 0 ? "" : `${next}px`;
      }
    };

    const schedule = (): void => {
      if (raf !== 0) return;
      raf = requestAnimationFrame(run);
    };

    const subs = [term.onWriteParsed?.(schedule), term.onCursorMove?.(schedule)];
    vv?.addEventListener("resize", schedule);
    vv?.addEventListener("scroll", schedule);
    // The Watch box itself changing size (Firefox layout-resize, rotation, a sheet re-fit) is the
    // most direct signal; a visualViewport event is not guaranteed to accompany it.
    const ro = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    ro?.observe(watch);
    window.addEventListener("resize", schedule);
    schedule();
    return () => {
      for (const sub of subs) sub?.dispose();
      vv?.removeEventListener("resize", schedule);
      vv?.removeEventListener("scroll", schedule);
      ro?.disconnect();
      window.removeEventListener("resize", schedule);
      if (raf !== 0) cancelAnimationFrame(raf);
      raf = 0;
      if (rearm !== null) clearTimeout(rearm);
      rearm = null;
      if (off !== 0) wrap.style.marginBottom = "";
    };
  }, [enabled, term, wrap, watch, gate]);
}
