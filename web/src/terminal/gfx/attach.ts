// T15 - the facade `TerminalPanel` talks to: one call wires the picture
// store, media cache, placeholder renderer and tap-to-open hit testing onto
// an xterm `Terminal`.
//
//   const gfx = attachGfx(term, { compact, bg, onOpenPicture });
//   frames:   imagedef -> gfx.onImageDef, gfxreset -> gfx.onGfxReset
//   joins:    gfx.onFullJoin() at the start of a FULL join (never a resume)
//   seed:     gfx.markLive() once the seed is applied
//   theme:    gfx.setTheme(bg)
//   teardown: gfx.dispose()
//
// Loading policy: a def that arrives AFTER `markLive()` (a picture drawn
// right now) starts loading at once; a def from the seed batch loads only
// when its id has placeholder cells inside the prefetch window (one screen
// above the viewport through one below), re-evaluated as the user scrolls.
// A long scrollback with hundreds of pictures therefore never fetches them
// all at join.

import type { Terminal } from "@xterm/xterm";
import type { ImageDefFrame } from "../wire";
import { ImageDefStore } from "./imageDefs";
import { FAILURE_COOLDOWN_MS, idsToPrefetch, MediaCache, prefetchWindow, type MediaCacheOptions } from "./media";
import { normalizeBg, PlaceholderRenderer, type GfxTerm, type Metrics } from "./renderer";

/** A tap on a picture: what the lightbox needs to show it. */
export interface OpenPicture {
  id: number;
  sha: string;
  mime: string;
  w: number;
  h: number;
}

export interface AttachGfxOptions {
  /** `profile.scrollback <= 20000`: the smaller media budget. */
  compact: boolean;
  /** Terminal background (any CSS colour). */
  bg: string;
  onOpenPicture?: (p: OpenPicture) => void;
  fetchImpl?: typeof fetch;
  /** Test seams. */
  mediaOptions?: MediaCacheOptions;
  measure?: () => Metrics;
  now?: () => number;
  requestFrame?: (cb: () => void) => number;
  cancelFrame?: (handle: number) => void;
}

export interface GfxHandle {
  onImageDef(f: ImageDefFrame): void;
  onGfxReset(gen: number): void;
  /** FULL join only: store.clear() + media.forgetAll() + re-render. Never on a resume. */
  onFullJoin(): void;
  /** The seed is applied: later defs load immediately. */
  markLive(): void;
  setTheme(bg: string): void;
  dispose(): void;
  /** For the lightbox. */
  readonly media: MediaCache;
  readonly defs: ImageDefStore;
  readonly renderer: PlaceholderRenderer;
}

/** A tap is a press shorter than this ... */
export const TAP_MAX_MS = 500;
/** ... that moves less than this many css pixels. */
export const TAP_MAX_MOVE_PX = 6;

type HostTerm = GfxTerm & { hasSelection(): boolean };

export function attachGfx(term: Terminal, opts: AttachGfxOptions): GfxHandle {
  // `Terminal` satisfies `GfxTerm` structurally except for the marker and
  // decoration option types (xterm's carry more members), hence the cast.
  const t = term as unknown as HostTerm;
  const now = opts.now ?? ((): number => performance.now());
  const defs = new ImageDefStore();
  const media = new MediaCache({ ...opts.mediaOptions, compact: opts.compact, fetchImpl: opts.fetchImpl });
  const renderer = new PlaceholderRenderer(t, defs, media, {
    bg: normalizeBg(opts.bg),
    measure: opts.measure,
    requestFrame: opts.requestFrame,
    cancelFrame: opts.cancelFrame,
  });
  const reqFrame = opts.requestFrame ?? ((cb: () => void): number => requestAnimationFrame(cb));
  const cancelFrame = opts.cancelFrame ?? ((h: number): void => cancelAnimationFrame(h));

  let live = false;
  let disposed = false;
  /** Seed-batch ids not yet requested. */
  const pendingSeed = new Set<number>();
  let prefetchRaf = 0;
  let openTimer: ReturnType<typeof setTimeout> | undefined;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;

  const prefetchNow = (): void => {
    if (disposed || pendingSeed.size === 0) return;
    const buf = t.buffer.active;
    const w = prefetchWindow(buf.viewportY, t.rows, buf.length);
    for (const id of idsToPrefetch(renderer.scan(w.start, w.count), [])) {
      if (!pendingSeed.has(id)) continue;
      pendingSeed.delete(id);
      const def = defs.get(id);
      if (def !== undefined) void media.load(def.sha);
    }
  };
  const schedulePrefetch = (): void => {
    if (disposed || pendingSeed.size === 0 || prefetchRaf !== 0) return;
    prefetchRaf = reqFrame(() => {
      prefetchRaf = 0;
      prefetchNow();
    });
  };

  const subs = [
    t.onScroll(schedulePrefetch),
    t.onRender(schedulePrefetch),
    defs.onChange(() => renderer.schedule()),
    media.onChange(() => {
      renderer.schedule();
      // A given-up load owes a retry after the cooldown even if the panel
      // sits idle: schedule one render for then (it re-asks via the cache).
      if (media.hasFailures && retryTimer === undefined && !disposed) {
        retryTimer = setTimeout(() => {
          retryTimer = undefined;
          if (!disposed) renderer.schedule();
        }, FAILURE_COOLDOWN_MS + 50);
      }
    }),
  ];

  // Tap to open: pointerdown/up on the screen element, capture + passive.
  const screen = t.element?.querySelector<HTMLElement>(".xterm-screen") ?? null;
  let down: { x: number; y: number; at: number; pointer: number } | null = null;
  const onDown = (ev: PointerEvent): void => {
    if (ev.button !== undefined && ev.button !== 0) return;
    down = { x: ev.clientX, y: ev.clientY, at: now(), pointer: ev.pointerId };
  };
  const onUp = (ev: PointerEvent): void => {
    const start = down;
    down = null;
    if (start === null || opts.onOpenPicture === undefined) return;
    if (ev.pointerId !== start.pointer) return;
    if (now() - start.at >= TAP_MAX_MS) return;
    if (Math.hypot(ev.clientX - start.x, ev.clientY - start.y) >= TAP_MAX_MOVE_PX) return;
    if (t.hasSelection()) return;
    const id = renderer.hitTest(ev.clientX, ev.clientY);
    if (id === null) return;
    const def = defs.get(id);
    if (def === undefined) return;
    const picture: OpenPicture = { id, sha: def.sha, mime: def.mime, w: def.w, h: def.h };
    // Open AFTER the `click` that follows this pointerup: the panel's host
    // click handler focuses the terminal, and a dialog opened (and focused)
    // synchronously would lose focus to it, leaving Esc going nowhere.
    openTimer = setTimeout(() => {
      openTimer = undefined;
      if (!disposed) opts.onOpenPicture?.(picture);
    }, 0);
  };
  screen?.addEventListener("pointerdown", onDown as EventListener, { capture: true, passive: true });
  screen?.addEventListener("pointerup", onUp as EventListener, { capture: true, passive: true });

  return {
    media,
    defs,
    renderer,
    onImageDef(f: ImageDefFrame): void {
      if (disposed) return;
      if (!defs.apply(f)) return;
      // A fresh announcement revives a sticky miss for these bytes.
      media.forget(f.sha256);
      if (live) void media.load(f.sha256);
      else {
        pendingSeed.add(f.id);
        schedulePrefetch();
      }
    },
    onGfxReset(gen: number): void {
      if (!disposed) defs.gfxReset(gen);
    },
    onFullJoin(): void {
      if (disposed) return;
      live = false;
      pendingSeed.clear();
      defs.clear();
      media.forgetAll();
      renderer.render();
    },
    markLive(): void {
      if (disposed) return;
      live = true;
      schedulePrefetch();
    },
    setTheme(bg: string): void {
      if (!disposed) renderer.setTheme(normalizeBg(bg));
    },
    dispose(): void {
      if (disposed) return;
      disposed = true;
      if (prefetchRaf !== 0) cancelFrame(prefetchRaf);
      prefetchRaf = 0;
      if (openTimer !== undefined) clearTimeout(openTimer);
      openTimer = undefined;
      if (retryTimer !== undefined) clearTimeout(retryTimer);
      retryTimer = undefined;
      for (const s of subs) {
        if (typeof s === "function") s();
        else s.dispose();
      }
      screen?.removeEventListener("pointerdown", onDown as EventListener, { capture: true });
      screen?.removeEventListener("pointerup", onUp as EventListener, { capture: true });
      renderer.dispose();
      media.dispose();
      pendingSeed.clear();
    },
  };
}
