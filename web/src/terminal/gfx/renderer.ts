// T15 - the kitty placeholder renderer: draw each picture over its cells.
//
// omp draws tool-result pictures as rows of U+10EEEE placeholder cells whose
// foreground colour carries the image id. Plain xterm shows them as tofu.
// This renderer scans the visible rows for those cells, groups them per
// picture, and lays one cropped `<img>` per group in an overlay inside
// `.xterm-screen`, so the picture appears exactly where the cells are and
// scrolls with them. It also recolours the placeholder glyphs to the
// background (a decoration per run) so no tofu or diacritic spills past the
// picture edge.
//
// Same algorithm as the spike the design was measured on; it never reads an
// xterm private field. Cell metrics come from an injectable `measure()`.
//
// not handled: the kitty placement id carried in SGR 58 (the tap already
// resolved placements into the ImageDef box), per-row-run groups for
// sparse or overwritten rows (a group is one rectangle of its visible
// cells), and z-order against text written over a picture.

import { decodePlaceholder, type Cell } from "./decode";
import { PH } from "./diacritics";
import type { ImageDefStore } from "./imageDefs";
import type { MediaCache } from "./media";

// ---- the slice of xterm this module touches (structural; `attach.ts`
// casts the real Terminal to it, tests pass a scripted fake) --------------

export interface GfxDisposable {
  dispose(): void;
}
export interface GfxMarker extends GfxDisposable {
  readonly line: number;
  readonly isDisposed: boolean;
}
export interface GfxCell {
  getChars(): string;
  isFgRGB(): boolean;
  isFgPalette(): boolean;
  getFgColor(): number;
}
export interface GfxLine {
  getCell(x: number, reuse?: GfxCell): GfxCell | undefined;
}
export interface GfxBuffer {
  readonly viewportY: number;
  readonly baseY: number;
  readonly cursorY: number;
  readonly length: number;
  getLine(y: number): GfxLine | undefined;
  getNullCell(): GfxCell;
}
export interface GfxDecorationOptions {
  marker: GfxMarker;
  x: number;
  width: number;
  height: number;
  foregroundColor: string;
  backgroundColor: string;
  layer: "top";
}
export interface GfxTerm {
  readonly element: HTMLElement | undefined;
  readonly rows: number;
  readonly cols: number;
  readonly buffer: { readonly active: GfxBuffer };
  onRender(cb: () => void): GfxDisposable;
  onScroll(cb: () => void): GfxDisposable;
  onResize(cb: () => void): GfxDisposable;
  registerMarker(cursorYOffset?: number): GfxMarker | undefined;
  registerDecoration(opts: GfxDecorationOptions): GfxDisposable | undefined;
}

/** What `measure()` reports: css cell size, device pixel ratio, buffer row at the viewport top. */
export interface Metrics {
  cw: number;
  ch: number;
  dpr: number;
  viewportY: number;
}

export interface ScannedCell extends Cell {
  /** Screen column and row of the cell, and its absolute buffer line. */
  x: number;
  y: number;
  abs: number;
}

export interface RendererStats {
  imgCreated: number;
  decosCreated: number;
  decosDisposed: number;
  frames: number;
}

export interface RendererOptions {
  /** Terminal background as `#rrggbb` (see `normalizeBg`). */
  bg: string;
  measure?: () => Metrics;
  /** rAF seam for tests. */
  requestFrame?: (cb: () => void) => number;
  cancelFrame?: (handle: number) => void;
}

type GroupKind = "img" | "missing" | "pending";

interface GroupEl {
  kind: GroupKind;
  clip: HTMLDivElement;
  inner: HTMLElement;
  /** Def version / object URL the `<img>` currently shows. */
  ver: number;
  src: string;
  seen: number;
}

interface Group {
  key: string;
  id: number;
  originAbs: number;
  originCol: number;
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
  maxRow: number;
  maxCol: number;
}

/** Black as `#rrggbb`. */
export const FALLBACK_BG = "#000000"; // kb-literal: xterm decoration colours accept only #rrggbb, so no var() token can stand here

/**
 * Normalise a CSS colour to the `#rrggbb` form xterm's decoration options
 * accept. Handles hex, `rgb()`/`rgba()`, and (in a browser) any other
 * syntax through a canvas round trip. Falls back to black.
 */
export function normalizeBg(css: string): string {
  const s = css.trim();
  const hex6 = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/i.exec(s);
  if (hex6) return `#${hex6[1].toLowerCase()}`;
  const hex3 = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/i.exec(s);
  if (hex3) return `#${hex3[1]}${hex3[1]}${hex3[2]}${hex3[2]}${hex3[3]}${hex3[3]}`.toLowerCase();
  const rgb = /^rgba?\(\s*(\d{1,3})[\s,]+(\d{1,3})[\s,]+(\d{1,3})/i.exec(s);
  if (rgb) {
    const to = (n: string): string => Math.min(255, Number(n)).toString(16).padStart(2, "0");
    return `#${to(rgb[1])}${to(rgb[2])}${to(rgb[3])}`;
  }
  if (s !== "" && typeof document !== "undefined") {
    try {
      const ctx = document.createElement("canvas").getContext("2d");
      if (ctx) {
        ctx.fillStyle = FALLBACK_BG;
        ctx.fillStyle = s;
        const out = ctx.fillStyle;
        const m = /^#([0-9a-f]{6})$/i.exec(out);
        if (m) return `#${m[1].toLowerCase()}`;
      }
    } catch {
      // no canvas: fall through
    }
  }
  return FALLBACK_BG;
}

export class PlaceholderRenderer {
  readonly stats: RendererStats = { imgCreated: 0, decosCreated: 0, decosDisposed: 0, frames: 0 };

  private readonly term: GfxTerm;
  private readonly defs: ImageDefStore;
  private readonly media: MediaCache;
  private readonly screen: HTMLElement;
  private readonly overlay: HTMLDivElement;
  private readonly measureFn: () => Metrics;
  private readonly reqFrame: (cb: () => void) => number;
  private readonly cancelFrame: (h: number) => void;
  private readonly subs: GfxDisposable[] = [];
  private readonly groups = new Map<string, GroupEl>();
  /** marker -> (run key -> decoration) */
  private readonly decos = new Map<GfxMarker, Map<string, GfxDisposable>>();
  private bg: string;
  private raf = 0;
  private disposed = false;

  constructor(term: GfxTerm, defs: ImageDefStore, media: MediaCache, opts: RendererOptions) {
    this.term = term;
    this.defs = defs;
    this.media = media;
    this.bg = opts.bg;
    const screen = term.element?.querySelector<HTMLElement>(".xterm-screen") ?? null;
    if (screen === null) throw new Error("PlaceholderRenderer: terminal is not open");
    this.screen = screen;
    this.measureFn = opts.measure ?? ((): Metrics => this.defaultMeasure());
    this.reqFrame = opts.requestFrame ?? ((cb: () => void) => requestAnimationFrame(cb));
    this.cancelFrame = opts.cancelFrame ?? ((h: number) => cancelAnimationFrame(h));
    const overlay = document.createElement("div");
    overlay.className = "kbgfx-overlay";
    screen.appendChild(overlay);
    this.overlay = overlay;
    // Paint in the frame xterm paints so text and pictures never disagree
    // by one frame; scroll/resize and store changes coalesce on a rAF.
    this.subs.push(term.onRender(() => this.render()));
    this.subs.push(term.onScroll(() => this.schedule()));
    this.subs.push(term.onResize(() => this.schedule()));
  }

  /** The overlay element (hit-testing and tests). */
  get overlayEl(): HTMLElement {
    return this.overlay;
  }

  /** Live decoration count (`created - disposed` must equal this). */
  get liveDecorations(): number {
    let n = 0;
    for (const m of this.decos.values()) n += m.size;
    return n;
  }

  /** The visible picture groups as screen-space css rects (lightbox hit-test). */
  hitTest(clientX: number, clientY: number): number | null {
    for (const el of this.groups.values()) {
      if (el.kind !== "img") continue;
      const r = el.clip.getBoundingClientRect();
      if (clientX >= r.left && clientX < r.right && clientY >= r.top && clientY < r.bottom) {
        return Number(el.clip.dataset.kbgfxId);
      }
    }
    return null;
  }

  schedule(): void {
    if (this.disposed || this.raf !== 0) return;
    this.raf = this.reqFrame(() => {
      this.raf = 0;
      this.render();
    });
  }

  /** Recolour for a new terminal background: decorations are rebuilt, clips repainted. */
  setTheme(bg: string): void {
    this.bg = bg;
    this.disposeAllDecorations();
    for (const el of this.groups.values()) el.clip.style.background = bg;
    this.render();
  }

  /** Placeholder cells in rows `[start, start + count)` of the buffer (absolute lines). */
  scan(start: number, count: number): ScannedCell[] {
    const buf = this.term.buffer.active;
    const out: ScannedCell[] = [];
    const reuse = buf.getNullCell();
    for (let y = 0; y < count; y += 1) {
      const line = buf.getLine(start + y);
      if (line === undefined) continue;
      let prev: Cell | null = null;
      for (let x = 0; x < this.term.cols; x += 1) {
        const c = line.getCell(x, reuse);
        if (c === undefined) break;
        const ch = c.getChars();
        if (ch.length >= 2 && ch.codePointAt(0) === PH) {
          const d = decodePlaceholder(ch, c.isFgRGB(), c.isFgPalette(), c.getFgColor(), prev);
          if (d !== null) {
            out.push({ ...d, x, y, abs: start + y });
            prev = d;
            continue;
          }
        }
        prev = null;
      }
    }
    return out;
  }

  render(): void {
    if (this.disposed) return;
    const buf = this.term.buffer.active;
    const cells = this.scan(buf.viewportY, this.term.rows);
    if (cells.length === 0 && this.groups.size === 0 && this.decos.size === 0) return;
    this.stats.frames += 1;
    const m = this.measureFn();
    const snap = (v: number): number => Math.round(v * m.dpr) / m.dpr;

    const groups = new Map<string, Group>();
    for (const c of cells) {
      const key = `${c.id}:${c.abs - c.row}:${c.x - c.col}`;
      let g = groups.get(key);
      if (g === undefined) {
        g = {
          key, id: c.id, originAbs: c.abs - c.row, originCol: c.x - c.col,
          minX: Infinity, maxX: -1, minY: Infinity, maxY: -1, maxRow: 0, maxCol: 0,
        };
        groups.set(key, g);
      }
      g.minX = Math.min(g.minX, c.x);
      g.maxX = Math.max(g.maxX, c.x);
      g.minY = Math.min(g.minY, c.y);
      g.maxY = Math.max(g.maxY, c.y);
      g.maxRow = Math.max(g.maxRow, c.row);
      g.maxCol = Math.max(g.maxCol, c.col);
    }

    for (const [key, g] of groups) {
      const def = this.defs.get(g.id);
      const loaded = def === undefined ? undefined : this.media.peek(def.sha);
      const kind: GroupKind =
        def === undefined || loaded === "missing"
          ? "missing"
          : loaded === undefined
            ? "pending"
            : "img";
      let el = this.groups.get(key);
      if (el !== undefined && el.kind !== kind) {
        el.clip.remove();
        this.groups.delete(key);
        el = undefined;
      }
      if (el === undefined) {
        el = this.createGroup(kind, g.id);
        this.groups.set(key, el);
      }
      el.seen = this.stats.frames;

      const left = snap(g.minX * m.cw);
      const top = snap(g.minY * m.ch);
      const right = snap((g.maxX + 1) * m.cw);
      const bottom = snap((g.maxY + 1) * m.ch);
      el.clip.style.left = `${left}px`;
      el.clip.style.top = `${top}px`;
      el.clip.style.width = `${right - left}px`;
      el.clip.style.height = `${bottom - top}px`;

      if (kind === "img" && def !== undefined && loaded !== undefined && loaded !== "missing") {
        // The FULL virtual picture box, which may extend past the clip
        // (cropped, never squashed).
        const C = def.cols > 0 ? def.cols : g.maxCol + 1;
        const R = def.rows > 0 ? def.rows : g.maxRow + 1;
        const boxL = snap(g.originCol * m.cw);
        const boxT = snap((g.originAbs - m.viewportY) * m.ch);
        const boxR = snap((g.originCol + C) * m.cw);
        const boxB = snap((g.originAbs + R - m.viewportY) * m.ch);
        el.inner.style.left = `${boxL - left}px`;
        el.inner.style.top = `${boxT - top}px`;
        el.inner.style.width = `${boxR - boxL}px`;
        el.inner.style.height = `${boxB - boxT}px`;
        const img = el.inner as HTMLImageElement;
        if (el.ver !== def.version || el.src !== loaded.url) {
          const first = el.ver === -1;
          img.src = loaded.url;
          el.ver = def.version;
          el.src = loaded.url;
          if (first) this.stats.imgCreated += 1;
        }
      } else {
        // Neutral box over the visible rectangle, not the whole virtual box.
        el.inner.style.left = "0px";
        el.inner.style.top = "0px";
        el.inner.style.width = `${right - left}px`;
        el.inner.style.height = `${bottom - top}px`;
      }
    }
    for (const [key, el] of [...this.groups]) {
      if (el.seen !== this.stats.frames) {
        el.clip.remove();
        this.groups.delete(key);
      }
    }
    this.syncDecorations(cells);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    if (this.raf !== 0) this.cancelFrame(this.raf);
    this.raf = 0;
    for (const s of this.subs) s.dispose();
    this.subs.length = 0;
    this.disposeAllDecorations();
    this.groups.clear();
    this.overlay.remove();
  }

  private defaultMeasure(): Metrics {
    const r = this.screen.getBoundingClientRect();
    return {
      cw: r.width / this.term.cols,
      ch: r.height / this.term.rows,
      dpr: window.devicePixelRatio || 1,
      viewportY: this.term.buffer.active.viewportY,
    };
  }

  private createGroup(kind: GroupKind, id: number): GroupEl {
    const clip = document.createElement("div");
    clip.className = "kbgfx-clip";
    clip.style.background = this.bg;
    clip.dataset.kbgfxId = String(id);
    let inner: HTMLElement;
    if (kind === "img") {
      const img = document.createElement("img");
      img.className = "kbgfx-img";
      img.draggable = false;
      img.alt = "";
      inner = img;
    } else {
      inner = document.createElement("div");
      inner.className = kind === "missing" ? "kbgfx-missing" : "kbgfx-pending";
      if (kind === "missing") inner.textContent = "image unavailable";
    }
    clip.appendChild(inner);
    this.overlay.appendChild(clip);
    return { kind, clip, inner, ver: -1, src: "", seen: 0 };
  }

  private disposeAllDecorations(): void {
    for (const m of this.decos.values()) {
      for (const d of m.values()) {
        d.dispose();
        this.stats.decosDisposed += 1;
      }
    }
    this.decos.clear();
  }

  /**
   * The U+10EEEE glyph (and its diacritics) is painted by xterm in the id
   * colour and can spill about a pixel past the cell. Recolour exactly the
   * placeholder runs to the background through the decoration API so nothing
   * is left to spill. One decoration per (line, contiguous run).
   */
  private syncDecorations(cells: readonly ScannedCell[]): void {
    const buf = this.term.buffer.active;
    const base = buf.viewportY;
    const rows = this.term.rows;
    const runs: Array<{ abs: number; x0: number; n: number }> = [];
    for (const c of cells) {
      const last = runs[runs.length - 1];
      if (last !== undefined && last.abs === c.abs && last.x0 + last.n === c.x) last.n += 1;
      else runs.push({ abs: c.abs, x0: c.x, n: 1 });
    }
    const wanted = new Map<number, Set<string>>();
    for (const r of runs) {
      let s = wanted.get(r.abs);
      if (s === undefined) {
        s = new Set();
        wanted.set(r.abs, s);
      }
      s.add(`${r.x0}:${r.n}`);
    }
    // Drop decorations whose marker died (the line was cleared or trimmed)
    // and, for visible lines, runs that are gone.
    for (const [marker, m] of [...this.decos]) {
      if (marker.isDisposed) {
        for (const d of m.values()) {
          d.dispose();
          this.stats.decosDisposed += 1;
        }
        this.decos.delete(marker);
        continue;
      }
      const abs = marker.line;
      if (abs < base || abs >= base + rows) continue;
      for (const [k, d] of [...m]) {
        if (wanted.get(abs)?.has(k) !== true) {
          d.dispose();
          m.delete(k);
          this.stats.decosDisposed += 1;
        }
      }
      if (m.size === 0) {
        marker.dispose();
        this.decos.delete(marker);
      }
    }
    const markerByLine = new Map<number, GfxMarker>();
    for (const marker of this.decos.keys()) markerByLine.set(marker.line, marker);
    const cursorAbs = buf.baseY + buf.cursorY;
    for (const r of runs) {
      const k = `${r.x0}:${r.n}`;
      let marker = markerByLine.get(r.abs);
      if (marker !== undefined && this.decos.get(marker)?.has(k) === true) continue;
      if (marker === undefined) {
        marker = this.term.registerMarker(r.abs - cursorAbs);
        if (marker === undefined) continue;
        this.decos.set(marker, new Map());
        markerByLine.set(r.abs, marker);
      }
      const d = this.term.registerDecoration({
        marker, x: r.x0, width: r.n, height: 1,
        foregroundColor: this.bg, backgroundColor: this.bg, layer: "top",
      });
      if (d !== undefined) {
        this.decos.get(marker)?.set(k, d);
        this.stats.decosCreated += 1;
      }
    }
  }
}
