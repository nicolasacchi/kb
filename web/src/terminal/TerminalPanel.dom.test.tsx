// @vitest-environment jsdom
// FUTERMFIT — the desktop panel's fit, chip, status line and theme wiring, with
// the xterm engine, the session machine and the picture layer faked (the real
// engine is covered by the e2e specs). Only the DOM seams the panel owns are
// real here: the host, its box, the toggle/resize observers and the controls.

import { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { TermRow } from "../api/terms";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

// ------------------------------------------------------------------ fakes

interface FakeTermInst {
  cols: number;
  rows: number;
  options: { fontSize: number; fontFamily: string; theme?: { background?: string } };
  element?: HTMLElement;
  clearTextureAtlas: ReturnType<typeof vi.fn>;
}
const terms: FakeTermInst[] = [];

vi.mock("@xterm/xterm", () => ({
  Terminal: class {
    cols: number;
    rows: number;
    options: FakeTermInst["options"];
    element: HTMLElement | undefined;
    unicode = { versions: ["15-graphemes"], activeVersion: "" };
    parser = { registerOscHandler: () => ({ dispose() {} }) };
    clearTextureAtlas = vi.fn();
    constructor(o: { cols: number; rows: number; fontSize: number; fontFamily: string; theme?: { background?: string } }) {
      this.cols = o.cols;
      this.rows = o.rows;
      this.options = { fontSize: o.fontSize, fontFamily: o.fontFamily, theme: o.theme };
      terms.push(this as unknown as FakeTermInst);
    }
    open(host: HTMLElement): void {
      const el = document.createElement("div");
      el.className = "xterm";
      const screen = document.createElement("div");
      screen.className = "xterm-screen";
      // The rendered screen is cols x cellW by rows x cellH per px of font size (the probe's cell).
      Object.defineProperty(screen, "scrollWidth", { get: () => this.cols * probe.cellW * this.options.fontSize });
      Object.defineProperty(screen, "scrollHeight", { get: () => this.rows * probe.cellH * this.options.fontSize });
      el.appendChild(screen);
      host.appendChild(el);
      this.element = el;
    }
    loadAddon(): void {}
    attachCustomKeyEventHandler(): void {}
    onData(): void {}
    onBinary(): void {}
    resize(): void {}
    reset(): void {}
    write(_d: unknown, done?: () => void): void {
      done?.();
    }
    focus(): void {}
    dispose(): void {}
  },
}));
vi.mock("@xterm/addon-image", () => ({ ImageAddon: class {} }));
vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: class { onContextLoss(): void {} } }));
vi.mock("@xterm/addon-unicode-graphemes", () => ({ UnicodeGraphemesAddon: class {} }));
vi.mock("@xterm/xterm/css/xterm.css", () => ({}));
vi.mock("./replyGuards", () => ({ installReplyGuards: () => {} }));
vi.mock("./pins", () => ({ profileMatchesBundle: () => true }));
vi.mock("./Lightbox", () => ({ Lightbox: () => null }));
vi.mock("./phone/PhoneTerminal", () => ({
  PhoneTerminal: () => null,
  attachTerm: (term: { element?: HTMLElement; open(h: HTMLElement): void }, host: HTMLElement) => {
    if (!term.element) term.open(host);
    else if (term.element.parentElement !== host) host.appendChild(term.element);
  },
}));
vi.mock("./phone/DriveDock", () => ({ DriveDock: () => null }));

const setGfxTheme = vi.fn();
vi.mock("./gfx", () => ({
  attachGfx: () => ({
    dispose() {},
    markLive() {},
    onImageDef() {},
    onGfxReset() {},
    onFullJoin() {},
    setTheme: (bg: string) => setGfxTheme(bg),
    media: {},
  }),
}));

interface SessionOpts {
  callbacks: { onProfile(p: unknown): void; onViewer(v: string): void; onState(s: unknown): void };
}
let lastSession: SessionOpts | null = null;
const sessionStart = vi.fn();
vi.mock("./session", () => ({
  TermSession: class {
    constructor(o: SessionOpts) {
      lastSession = o;
    }
    start = sessionStart;
    dispose(): void {}
    kick(): void {}
    sendInput(): boolean {
      return true;
    }
    block(): void {}
    rejoinFull(): void {}
  },
}));

let rows: Partial<TermRow>[] = [];
vi.mock("./useTerms", () => ({ useTerms: () => ({ terms: rows, isLoading: false }) }));

const probe = { cellW: 0.6, cellH: 1.32 };
vi.mock("./cellProbe", () => ({ measureCellRatio: () => ({ ...probe }) }));

let gate: { ready: Promise<boolean>; done: Promise<void> } = { ready: Promise.resolve(true), done: Promise.resolve() };
vi.mock("./fonts", async (orig) => ({ ...(await orig<typeof import("./fonts")>()), fontGate: () => gate }));

const readThemeMock = vi.fn();
vi.mock("./theme", () => ({ readTheme: (...a: unknown[]) => readThemeMock(...a) }));

const confirmMock = vi.fn(async () => true);
vi.mock("../components/ConfirmProvider", () => ({ useConfirm: () => confirmMock }));
vi.mock("../lib/toast", () => ({ toast: { err: vi.fn(), ok: vi.fn(), info: vi.fn() } }));

import { THEME_APPLIED_EVENT } from "@kb/ui/themes/apply";
import { TerminalPanel } from "./TerminalPanel";
import { rigFetch, settle, type FetchRig } from "./phone/driveTestkit";

// ---------------------------------------------------------------- harness

let host: HTMLElement | null = null;
let root: Root | null = null;
let rig: FetchRig;
let box = { w: 1400, h: 800 };
let roCallbacks: Array<() => void> = [];

const PROFILE = {
  t: "profile", xterm: "", serialize: "", width: "", image: "", cols: 201, rows: 48, scrollback: 50_000,
  scroll_on_erase_in_display: true, cell_px_w: 8, cell_px_h: 17, theme: null,
};

beforeEach(() => {
  terms.length = 0;
  lastSession = null;
  sessionStart.mockReset();
  setGfxTheme.mockReset();
  confirmMock.mockClear();
  gate = { ready: Promise.resolve(true), done: Promise.resolve() };
  box = { w: 1400, h: 800 };
  probe.cellW = 0.6;
  probe.cellH = 1.32;
  rows = [{ id: "abcd1234", state: "working", grid_owner: null }];
  readThemeMock.mockReset();
  readThemeMock.mockReturnValue({ background: "#111111" });
  roCallbacks = [];
  rig = rigFetch();
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => box.w });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => box.h });
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
    constructor(cb: () => void) {
      roCallbacks.push(cb);
    }
    observe(): void {}
    disconnect(): void {}
  };
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  host = root = null;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientWidth;
  delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientHeight;
  vi.unstubAllGlobals();
});

async function mountPanel(): Promise<void> {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const qc = new QueryClient();
  act(() =>
    root!.render(
      <QueryClientProvider client={qc}>
        <TerminalPanel termId="abcd1234" sid="s1" harness="omp" mode="watch" />
      </QueryClientProvider>,
    ),
  );
  await settle(4);
}

async function profile(): Promise<FakeTermInst> {
  act(() => lastSession!.callbacks.onProfile(PROFILE));
  await settle(2);
  return terms[0];
}

const q = (id: string): HTMLElement | null => host!.querySelector<HTMLElement>(`[data-testid="${id}"]`);
const text = (id: string): string => q(id)?.textContent ?? "";

// ------------------------------------------------------------------ tests

describe("fit", () => {
  it("fits the whole grid at the first Profile (font 11 for 201x48 in 1400x800) and the chip says so", async () => {
    await mountPanel();
    const term = await profile();
    expect(term.options.fontSize).toBe(11);
    expect(text("kb-term-grid")).toBe("201x48 · free");
    expect(text("kb-term-font")).toBe("font 11px");
    expect(q("kb-term-cropped")).toBeNull();
  });

  it("shows the Cropped hint when the font floor still cannot hold the grid", async () => {
    box = { w: 800, h: 800 };
    await mountPanel();
    const term = await profile();
    expect(term.options.fontSize).toBe(9);
    expect(text("kb-term-font")).toBe("font 9px");
    expect(q("kb-term-cropped")).not.toBeNull();
  });

  it("never fits at zero width, then fits when the host gets a box (the closed <details> case)", async () => {
    box = { w: 0, h: 0 };
    await mountPanel();
    const term = await profile();
    const initial = term.options.fontSize;
    expect(q("kb-term-font")).toBeNull();
    box = { w: 1400, h: 800 };
    act(() => roCallbacks.forEach((cb) => cb()));
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    expect(initial).not.toBe(11);
    expect(term.options.fontSize).toBe(11);
  });

  it("the stream opens only after the font gate; a face that loads LATE re-assigns the family, clears the atlas and refits", async () => {
    let finish: () => void = () => {};
    gate = {
      ready: Promise.resolve(false),
      done: new Promise<void>((r) => {
        finish = r;
      }),
    };
    await mountPanel();
    expect(sessionStart).toHaveBeenCalledTimes(1);
    const term = await profile();
    const family = term.options.fontFamily;
    expect(term.options.fontSize).toBe(11);
    // The real face is narrower and shorter: a re-measure gives a bigger font.
    probe.cellW = 0.5;
    probe.cellH = 1.2;
    finish();
    await settle(4);
    expect(term.options.fontFamily).not.toBe(family);
    expect(term.options.fontFamily.startsWith(family)).toBe(true);
    expect(term.clearTextureAtlas).toHaveBeenCalled();
    expect(term.options.fontSize).toBe(13);
    expect(text("kb-term-font")).toBe("font 13px");
  });

  it("does not open the stream before the gate resolves", async () => {
    let open: (v: boolean) => void = () => {};
    gate = { ready: new Promise<boolean>((r) => (open = r)), done: Promise.resolve() };
    await mountPanel();
    expect(sessionStart).not.toHaveBeenCalled();
    open(true);
    await settle(4);
    expect(sessionStart).toHaveBeenCalledTimes(1);
  });

  it("a window resize refits the font and sends NO take-size or resize", async () => {
    await mountPanel();
    const term = await profile();
    expect(term.options.fontSize).toBe(11);
    box = { w: 1900, h: 1000 };
    act(() => {
      window.dispatchEvent(new Event("resize"));
      roCallbacks.forEach((cb) => cb());
    });
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    expect(term.options.fontSize).toBe(15);
    expect(rig.calls).toHaveLength(0);
  });
});

describe("chip and controls", () => {
  it("names the owner in the chip: web, local attach, another owner", async () => {
    await mountPanel();
    await profile();
    for (const [owner, label] of [
      ["kb-term", "201x48 · web"],
      ["local:9", "201x48 · local attach"],
      ["desk-1", "201x48 · desk-1"],
      [null, "201x48 · free"],
    ] as const) {
      rows = [{ id: "abcd1234", state: "working", grid_owner: owner }];
      act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="abcd1234" sid="s1" harness="omp" mode="watch" /></QueryClientProvider>));
      expect(text("kb-term-grid")).toBe(label);
    }
  });

  it("claims no owner while the list has no row for the term (unknown is not free)", async () => {
    rows = [];
    await mountPanel();
    await profile();
    expect(text("kb-term-grid")).not.toContain("free");
    expect(text("kb-term-grid")).not.toContain("·");
  });

  it("dims the panel while it is not live", async () => {
    await mountPanel();
    expect(q("kb-terminal")!.className).toContain("kb-term--idle");
    act(() => lastSession!.callbacks.onState({ phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null }));
    expect(q("kb-terminal")!.className).not.toContain("kb-term--idle");
  });

  it("Fit to window posts the measured dims with the hello's viewer and reports in the aria-live status line inside the panel", async () => {
    rows = [{ id: "abcd1234", state: "working", grid_owner: "local:9" }];
    await mountPanel();
    await profile();
    act(() => lastSession!.callbacks.onViewer("ws-viewer00001"));
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    // 1400 / (0.6 * 14) and 800 / (1.32 * 14)
    expect(rig.calls[0].json).toEqual({ cols: 166, rows: 43, viewer: "ws-viewer00001" });
    expect(confirmMock).not.toHaveBeenCalled();
    const status = q("kb-term-status")!;
    expect(status.getAttribute("aria-live")).toBe("polite");
    expect(status.closest(".kb-term")).not.toBeNull();
    expect(status.textContent).toBe("Took the grid from the local attach; it now watches (166x43).");
  });

  it("Release grid shows only for owner kb-term and posts release-size", async () => {
    rows = [{ id: "abcd1234", state: "working", grid_owner: "kb-term" }];
    await mountPanel();
    await profile();
    expect(q("kb-term-release")).not.toBeNull();
    act(() => q("kb-term-release")!.click());
    await settle(10);
    expect(rig.urls()).toEqual(["/api/terms/abcd1234/release-size"]);
    expect(text("kb-term-status")).toBe("Released the grid.");
  });

  it("when the list cannot say who owns the grid, the runner's answer to our own take names the web, and Release frees it", async () => {
    rows = [];
    await mountPanel();
    await profile();
    expect(text("kb-term-grid")).toBe("201x48");
    expect(q("kb-term-release")).toBeNull();
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · web");
    expect(q("kb-term-release")).not.toBeNull();
    act(() => q("kb-term-release")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · free");
    expect(q("kb-term-release")).toBeNull();
  });

  it("a change of the list row's owner retires the hint (a local attach that took the grid after us shows)", async () => {
    rows = [{ id: "abcd1234", state: "working", grid_owner: null }];
    await mountPanel();
    await profile();
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · web");
    rows = [{ id: "abcd1234", state: "working", grid_owner: "local:9" }];
    act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="abcd1234" sid="s1" harness="omp" mode="watch" /></QueryClientProvider>));
    await settle(2);
    expect(text("kb-term-grid")).toBe("201x48 · local attach");
  });

  it("has exactly one Close (the panel's own)", async () => {
    await mountPanel();
    expect(host!.querySelectorAll('[data-testid="kb-term-close"]')).toHaveLength(1);
  });
});

describe("theme", () => {
  it("re-reads the theme when the SPA theme changes, repainting the host and the picture layer", async () => {
    await mountPanel();
    const term = await profile();
    expect(term.options.theme).toEqual({ background: "#111111" });
    readThemeMock.mockReturnValue({ background: "#fafafa" });
    act(() => {
      window.dispatchEvent(new Event(THEME_APPLIED_EVENT));
    });
    expect(term.options.theme).toEqual({ background: "#fafafa" });
    expect(readThemeMock).toHaveBeenCalledTimes(2);
    expect((host!.querySelector("[data-kb-term]") as HTMLElement).style.background).toBe("rgb(250, 250, 250)");
    expect(setGfxTheme).toHaveBeenCalledWith("#fafafa");
  });

  it("the host starts with the terminal's own background", async () => {
    await mountPanel();
    await profile();
    expect((host!.querySelector("[data-kb-term]") as HTMLElement).style.background).toBe("rgb(17, 17, 17)");
  });
});
