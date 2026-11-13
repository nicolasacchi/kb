// Test support for the placeholder renderer / attach facade: a scripted
// xterm stand-in (buffer lines, markers, decorations, event listeners).
// Not imported by production code (the `lazy.test.ts` walk skips *.test.*
// only, and this file is under terminal/gfx so it is inside the allowed tree).

import { DIAC, PH } from "./diacritics";
import type {
  GfxDecorationOptions,
  GfxDisposable,
  GfxMarker,
  GfxTerm,
} from "./renderer";

export interface FakeCell {
  ch: string;
  fg?: number;
}

export class FakeMarker implements GfxMarker {
  isDisposed = false;
  constructor(public line: number) {}
  dispose(): void {
    this.isDisposed = true;
  }
}

export interface FakeDecoration extends GfxDisposable {
  opts: GfxDecorationOptions;
  disposed: boolean;
}

export const ph = (row: number, col: number): string =>
  String.fromCodePoint(PH) + String.fromCodePoint(DIAC[row]) + String.fromCodePoint(DIAC[col]);

export class FakeTerm {
  readonly element: HTMLElement;
  readonly screen: HTMLElement;
  viewportY = 0;
  baseY = 0;
  cursorY = 0;
  lines: FakeCell[][] = [];
  markers: FakeMarker[] = [];
  decorations: FakeDecoration[] = [];
  selection = false;
  private readonly listeners = { render: new Set<() => void>(), scroll: new Set<() => void>(), resize: new Set<() => void>() };

  constructor(public cols = 20, public rows = 10) {
    this.element = document.createElement("div");
    this.screen = document.createElement("div");
    this.screen.className = "xterm-screen";
    this.element.appendChild(this.screen);
    document.body.appendChild(this.element);
  }

  get listenerCount(): number {
    return this.listeners.render.size + this.listeners.scroll.size + this.listeners.resize.size;
  }

  get liveDecorations(): number {
    return this.decorations.filter((d) => !d.disposed).length;
  }

  asTerm(): GfxTerm & { hasSelection(): boolean } {
    const self = this;
    const sub = (set: Set<() => void>) => (cb: () => void): GfxDisposable => {
      set.add(cb);
      return { dispose: () => void set.delete(cb) };
    };
    return {
      element: self.element,
      get rows() {
        return self.rows;
      },
      get cols() {
        return self.cols;
      },
      buffer: {
        active: {
          get viewportY() {
            return self.viewportY;
          },
          get baseY() {
            return self.baseY;
          },
          get cursorY() {
            return self.cursorY;
          },
          get length() {
            return Math.max(self.lines.length, self.baseY + self.rows);
          },
          getLine: (y: number) => {
            const line = self.lines[y];
            if (line === undefined) return undefined;
            return {
              getCell: (x: number) => {
                const c = line[x] ?? { ch: "" };
                return {
                  getChars: () => c.ch,
                  isFgRGB: () => c.fg !== undefined,
                  isFgPalette: () => false,
                  getFgColor: () => c.fg ?? 0,
                };
              },
            };
          },
          getNullCell: () => ({
            getChars: () => "",
            isFgRGB: () => false,
            isFgPalette: () => false,
            getFgColor: () => 0,
          }),
        },
      },
      onRender: sub(self.listeners.render),
      onScroll: sub(self.listeners.scroll),
      onResize: sub(self.listeners.resize),
      registerMarker: (offset?: number) => {
        const m = new FakeMarker(self.baseY + self.cursorY + (offset ?? 0));
        self.markers.push(m);
        return m;
      },
      registerDecoration: (opts: GfxDecorationOptions) => {
        const d: FakeDecoration = {
          opts,
          disposed: false,
          dispose() {
            d.disposed = true;
          },
        };
        self.decorations.push(d);
        return d;
      },
      hasSelection: () => self.selection,
    };
  }

  fire(kind: "render" | "scroll" | "resize"): void {
    for (const cb of [...this.listeners[kind]]) cb();
  }

  /** Write a `cols x rows` picture of placeholder cells at (absLine, col). */
  picture(id: number, absLine: number, col: number, cols: number, rows: number): void {
    for (let r = 0; r < rows; r += 1) {
      const line = (this.lines[absLine + r] ??= []);
      for (let c = 0; c < cols; c += 1) line[col + c] = { ch: ph(r, c), fg: id };
    }
  }

  text(absLine: number, col: number, s: string): void {
    const line = (this.lines[absLine] ??= []);
    [...s].forEach((ch, i) => {
      line[col + i] = { ch };
    });
  }

  /** ED2/ED3-like repaint: every marker dies (xterm disposes their decorations), lines reset. */
  clearAll(): void {
    for (const m of this.markers) m.dispose();
    for (const d of this.decorations) d.dispose();
    this.lines = [];
  }
}
