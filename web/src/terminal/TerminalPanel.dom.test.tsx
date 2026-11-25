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
  options: {
    fontSize: number;
    fontFamily: string;
    theme?: { background?: string };
    minimumContrastRatio?: number;
    drawBoldTextInBrightColors?: boolean;
    customGlyphs?: boolean;
    screenReaderMode?: boolean;
  };
  element?: HTMLElement;
  clearTextureAtlas: ReturnType<typeof vi.fn>;
  keyHandler?: (ev: Record<string, unknown>) => boolean;
  onDataCb?: (d: string) => void;
  linkProvider?: { provideLinks(y: number, cb: (l?: Array<Record<string, unknown>>) => void): void };
  scrollCbs: Array<() => void>;
  bellCbs: Array<() => void>;
  titleCbs: Array<(t: string) => void>;
  selection: string;
  buf: { viewportY: number; baseY: number; length: number; lines: Array<{ text: string; wrapped: boolean }> };
  scrollToBottom: ReturnType<typeof vi.fn>;
  focus: ReturnType<typeof vi.fn>;
  select: ReturnType<typeof vi.fn>;
  paste: ReturnType<typeof vi.fn>;
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
    keyHandler: ((ev: Record<string, unknown>) => boolean) | undefined;
    onDataCb: ((d: string) => void) | undefined;
    linkProvider: FakeTermInst["linkProvider"];
    scrollCbs: Array<() => void> = [];
    bellCbs: Array<() => void> = [];
    titleCbs: Array<(t: string) => void> = [];
    selection = "";
    buf = { viewportY: 0, baseY: 0, length: 1, lines: [] as Array<{ text: string; wrapped: boolean }> };
    scrollToBottom = vi.fn();
    focus = vi.fn();
    select = vi.fn();
    paste = vi.fn();
    get buffer() {
      const b = this.buf;
      return {
        active: {
          viewportY: b.viewportY,
          baseY: b.baseY,
          length: b.lines.length || b.length,
          getLine: (i: number) => {
            const l = b.lines[i];
            return l ? { translateToString: () => l.text, isWrapped: l.wrapped } : undefined;
          },
        },
      };
    }
    hasSelection(): boolean {
      return this.selection !== "";
    }
    clearSelection(): void {
      this.selection = "";
    }
    getSelection(): string {
      return this.selection;
    }
    registerLinkProvider(p: FakeTermInst["linkProvider"]): void {
      this.linkProvider = p;
    }
    onScroll(cb: () => void): void {
      this.scrollCbs.push(cb);
    }
    onWriteParsed(cb: () => void): void {
      this.scrollCbs.push(cb);
    }
    onBell(cb: () => void): void {
      this.bellCbs.push(cb);
    }
    onTitleChange(cb: (t: string) => void): void {
      this.titleCbs.push(cb);
    }
    scrollLines(): void {}
    constructor(o: {
      cols: number;
      rows: number;
      fontSize: number;
      fontFamily: string;
      theme?: { background?: string };
      minimumContrastRatio?: number;
      drawBoldTextInBrightColors?: boolean;
      customGlyphs?: boolean;
      screenReaderMode?: boolean;
    }) {
      this.cols = o.cols;
      this.rows = o.rows;
      this.options = {
        fontSize: o.fontSize,
        fontFamily: o.fontFamily,
        theme: o.theme,
        minimumContrastRatio: o.minimumContrastRatio,
        drawBoldTextInBrightColors: o.drawBoldTextInBrightColors,
        customGlyphs: o.customGlyphs,
        screenReaderMode: o.screenReaderMode,
      };
      terms.push(this as unknown as FakeTermInst);
    }
    open(host: HTMLElement): void {
      const el = document.createElement("div");
      el.className = "xterm";
      const screen = document.createElement("div");
      screen.className = "xterm-screen";
      // The rendered screen is cols x cellW by rows x cellH per px of font size (the probe's cell).
      Object.defineProperty(screen, "offsetWidth", { get: () => this.cols * probe.cellW * this.options.fontSize });
      Object.defineProperty(screen, "offsetHeight", { get: () => this.rows * probe.cellH * this.options.fontSize });
      el.appendChild(screen);
      host.appendChild(el);
      this.element = el;
    }
    loadAddon(): void {}
    attachCustomKeyEventHandler(h: (ev: Record<string, unknown>) => boolean): void {
      this.keyHandler = h;
    }
    onData(cb: (d: string) => void): void {
      this.onDataCb = cb;
    }
    onBinary(): void {}
    resize(cols: number, rows: number): void {
      this.cols = cols;
      this.rows = rows;
    }
    reset(): void {}
    write(_d: unknown, done?: () => void): void {
      done?.();
    }
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
const gfxMetrics = vi.fn();
vi.mock("./gfx", () => ({
  attachGfx: () => ({
    dispose() {},
    notifyMetricsChanged: () => gfxMetrics(),
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
  sink: { resize(cols: number, rows: number): void };
  effects: {
    openSocket(
      url: string,
      handlers: { onMessageBinary(b: Uint8Array): void; onMessageText(t: string): void; onClose(code: number, reason: string): void },
    ): unknown;
  };
}
let lastSession: SessionOpts | null = null;
const sessionStart = vi.fn();
const kickMock = vi.fn();
const sendInputMock = vi.fn();
const setHiddenMock = vi.fn();
vi.mock("./session", () => ({
  TermSession: class {
    constructor(o: SessionOpts) {
      lastSession = o;
    }
    start = sessionStart;
    dispose(): void {}
    kick(): void {
      kickMock();
    }
    sendInput(d: string): boolean {
      sendInputMock(d);
      return true;
    }
    block(): void {}
    setHidden(h: boolean): void {
      setHiddenMock(h);
    }
    rejoinFull(): void {}
  },
}));

let liveRows: Array<Record<string, unknown>> = [];
vi.mock("../hooks/useLiveSessions", () => ({ useLiveSessions: () => ({ rows: liveRows, loading: false }) }));

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
import { resetInitialInput } from "./initialInput";
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
  window.localStorage.clear();
  terms.length = 0;
  lastSession = null;
  sessionStart.mockReset();
  kickMock.mockReset();
  sendInputMock.mockReset();
  setGfxTheme.mockReset();
  gfxMetrics.mockReset();
  setHiddenMock.mockReset();
  confirmMock.mockClear();
  gate = { ready: Promise.resolve(true), done: Promise.resolve() };
  box = { w: 1400, h: 800 };
  probe.cellW = 0.6;
  probe.cellH = 1.32;
  rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null }];
  liveRows = [];
  resetInitialInput();
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
        <TerminalPanel termId="abcd1234" sid="s1" harness="omp" />
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
      rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: owner }];
      act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="abcd1234" sid="s1" harness="omp" /></QueryClientProvider>));
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
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: "local:9" }];
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
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: "kb-term" }];
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
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null }];
    await mountPanel();
    await profile();
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · web");
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: "local:9" }];
    act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="abcd1234" sid="s1" harness="omp" /></QueryClientProvider>));
    await settle(2);
    expect(text("kb-term-grid")).toBe("201x48 · local attach");
  });

  it("has exactly one Close (the panel's own)", async () => {
    await mountPanel();
    expect(host!.querySelectorAll('[data-testid="kb-term-close"]')).toHaveLength(1);
  });
});

describe("theme", () => {
  it("constructs the terminal with a minimum contrast ratio, explicit bright colours and custom glyphs", async () => {
    await mountPanel();
    const term = await profile();
    expect(term.options.minimumContrastRatio).toBe(4.5);
    expect(term.options.drawBoldTextInBrightColors).toBe(false);
    expect(term.options.customGlyphs).toBe(true);
  });

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

// ----------------------------------------------------------- FUTERMFULL

const keyEv = (o: Record<string, unknown>) => ({
  type: "keydown",
  key: "c",
  ctrlKey: false,
  metaKey: false,
  shiftKey: false,
  altKey: false,
  preventDefault: vi.fn(),
  ...o,
});

describe("input state", () => {
  it("starts as 'click to type' with no focus() on mount, says 'typing' on focus and back on blur", async () => {
    await mountPanel();
    const term = await profile();
    expect(term.focus).not.toHaveBeenCalled();
    expect(text("kb-term-input")).toBe("click to type");
    expect(q("kb-terminal")!.className).not.toContain("kb-term--typing");
    const view = host!.querySelector("[data-kb-term]")!;
    act(() => view.dispatchEvent(new FocusEvent("focusin", { bubbles: true })));
    expect(text("kb-term-input")).toBe("typing");
    expect(q("kb-terminal")!.className).toContain("kb-term--typing");
    act(() => view.dispatchEvent(new FocusEvent("focusout", { bubbles: true })));
    expect(text("kb-term-input")).toBe("click to type");
  });
});

describe("copy, paste and links", () => {
  it("Ctrl+C with a selection copies it and sends nothing; without one the key is left to xterm (^C)", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await mountPanel();
    const term = await profile();
    term.selection = "picked text";
    const withSel = keyEv({ ctrlKey: true });
    expect(term.keyHandler!(withSel)).toBe(false);
    expect(withSel.preventDefault).toHaveBeenCalled();
    expect(writeText).toHaveBeenCalledWith("picked text");
    term.selection = "";
    const none = keyEv({ ctrlKey: true });
    expect(term.keyHandler!(none)).toBe(true);
    expect(none.preventDefault).not.toHaveBeenCalled();
    expect(writeText).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+Shift+C copies and Ctrl+Shift+V is the browser's paste (swallowed by xterm, not prevented)", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await mountPanel();
    const term = await profile();
    term.selection = "abc";
    expect(term.keyHandler!(keyEv({ ctrlKey: true, shiftKey: true, key: "C" }))).toBe(false);
    expect(writeText).toHaveBeenCalledWith("abc");
    const paste = keyEv({ ctrlKey: true, shiftKey: true, key: "V" });
    expect(term.keyHandler!(paste)).toBe(false);
    expect(paste.preventDefault).not.toHaveBeenCalled();
  });

  it("the link provider returns a wrapped URL on both rows and Ctrl-click opens it with noopener,noreferrer", async () => {
    const open = vi.spyOn(window, "open").mockImplementation(() => null);
    await mountPanel();
    const term = await profile();
    term.buf.lines = [
      { text: "x".repeat(188) + " https://exam", wrapped: false },
      { text: "ple.com/p done", wrapped: true },
    ];
    // cols = 201: the first row is 201 wide once padded.
    const got: Array<Array<Record<string, unknown>> | undefined> = [];
    term.linkProvider!.provideLinks(2, (l) => got.push(l));
    expect(got[0]).toBeDefined();
    const link = got[0]![0] as { text: string; activate(e: Partial<MouseEvent>, t: string): void };
    expect(link.text).toBe("https://example.com/p");
    link.activate({ ctrlKey: false, metaKey: false }, link.text);
    expect(open).not.toHaveBeenCalled();
    link.activate({ ctrlKey: true }, link.text);
    expect(open).toHaveBeenCalledWith("https://example.com/p", "_blank", "noopener,noreferrer");
    const none: Array<unknown> = [];
    term.buf.lines = [{ text: "no links here", wrapped: false }];
    term.linkProvider!.provideLinks(1, (l) => none.push(l));
    expect(none).toEqual([undefined]);
    open.mockRestore();
  });
});

describe("FUPHONELINKS desktop link hint", () => {
  it("hovering a link says how to open it and leaving clears the hint", async () => {
    await mountPanel();
    const term = await profile();
    term.buf.lines = [{ text: "go https://a.dev/x now", wrapped: false }];
    const got: Array<Array<Record<string, unknown>> | undefined> = [];
    term.linkProvider!.provideLinks(1, (l) => got.push(l));
    const link = got[0]![0] as { text: string; hover(e: unknown, t: string): void; leave(e: unknown, t: string): void };
    const el = (term as unknown as { element: HTMLElement }).element;
    expect(el.title).toBe("");
    link.hover({}, link.text);
    expect(el.title).toBe("Ctrl+click to open");
    link.leave({}, link.text);
    expect(el.title).toBe("");
  });
});

describe("Jump to latest", () => {
  it("shows only while the view is scrolled up, and jumps to the bottom", async () => {
    await mountPanel();
    const term = await profile();
    expect(q("kb-term-jump")).toBeNull();
    term.buf.baseY = 100;
    term.buf.viewportY = 20;
    act(() => term.scrollCbs.forEach((cb) => cb()));
    expect(q("kb-term-jump")).not.toBeNull();
    act(() => q("kb-term-jump")!.click());
    expect(term.scrollToBottom).toHaveBeenCalled();
    term.buf.viewportY = 100;
    act(() => term.scrollCbs.forEach((cb) => cb()));
    expect(q("kb-term-jump")).toBeNull();
  });
});

describe("find", () => {
  it("a new query starts from the first match, not from the previous hit", async () => {
    await mountPanel();
    const term = await profile();
    term.buf.lines = [{ text: "alpha one", wrapped: false }, { text: "beta two", wrapped: false }, { text: "alpha three", wrapped: false }];
    act(() => q("kb-term-find-toggle")!.click());
    const input = q("kb-term-find-input") as HTMLInputElement;
    const setValue = (v: string): void => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      act(() => {
        setter.call(input, v);
        input.dispatchEvent(new Event("input", { bubbles: true }));
      });
    };
    const enter = (): void => act(() => void input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })));
    setValue("alpha");
    enter();
    enter();
    expect(term.select).toHaveBeenLastCalledWith(0, 2, 5);
    // Fresh query "a": from the top the first hit is line 0 col 0. A cursor left at the
    // previous hit (line 2, col 0) would continue to line 2 col 4 instead.
    setValue("a");
    enter();
    expect(term.select).toHaveBeenLastCalledWith(0, 0, 1);
    expect(text("kb-term-find-note")).toBe("5 matches");
  });

  it("closing Find returns focus to the terminal", async () => {
    await mountPanel();
    const term = await profile();
    act(() => q("kb-term-find-toggle")!.click());
    const input = q("kb-term-find-input")!;
    act(() => void input.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    expect(q("kb-term-find-input")).toBeNull();
    expect(term.focus).toHaveBeenCalled();
  });
});

describe("full screen", () => {
  it("the overlay: the button toggles .kb-term--max and its label, and an Escape reaches the agent and leaves it open", async () => {
    await mountPanel();
    const term = await profile();
    const panel = q("kb-terminal")!;
    expect(text("kb-term-fullscreen")).toBe("Full screen");
    await act(async () => q("kb-term-fullscreen")!.click());
    await settle(2);
    expect(panel.classList.contains("kb-term--max")).toBe(true);
    expect(text("kb-term-fullscreen")).toBe("Exit full screen");
    expect(term.focus).toHaveBeenCalled();
    // A React re-render of the panel's className (the focus chip, the idle state) must not wipe the overlay class.
    act(() => host!.querySelector("[data-kb-term]")!.dispatchEvent(new FocusEvent("focusin", { bubbles: true })));
    expect(panel.className).toContain("kb-term--typing");
    expect(panel.classList.contains("kb-term--max")).toBe(true);
    act(() => lastSession!.callbacks.onState({ phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null }));
    expect(panel.classList.contains("kb-term--max")).toBe(true);
    // Escape: xterm's own key path (the handler lets it through) -> onData -> the session's input.
    const esc = keyEv({ key: "Escape" });
    expect(term.keyHandler!(esc)).toBe(true);
    term.onDataCb!("\u001b");
    expect(sendInputMock).toHaveBeenCalledWith("\u001b");
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
      host!.querySelector("[data-kb-term]")!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(panel.classList.contains("kb-term--max")).toBe(true);
    await act(async () => q("kb-term-fullscreen")!.click());
    await settle(2);
    expect(panel.classList.contains("kb-term--max")).toBe(false);
  });

  it("refits with the full-screen bounds (min 8, max 28) and sends no take-size or resize", async () => {
    box = { w: 2800, h: 1500 };
    await mountPanel();
    const term = await profile();
    // inline: clamped to 20
    expect(term.options.fontSize).toBe(20);
    await act(async () => q("kb-term-fullscreen")!.click());
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    // 2800 / (0.6 * 201) = 23.2, 1500 / (1.32 * 48) = 23.67
    expect(term.options.fontSize).toBe(23);
    expect(rig.calls).toHaveLength(0);
  });

  it("closing the panel leaves full screen; so does unmounting it", async () => {
    await mountPanel();
    await profile();
    const panel = q("kb-terminal")!;
    await act(async () => q("kb-term-fullscreen")!.click());
    await settle(2);
    expect(panel.classList.contains("kb-term--max")).toBe(true);
    act(() => root!.unmount());
    expect(panel.classList.contains("kb-term--max")).toBe(false);
    root = createRoot(host!);
  });

  it("a term id change drops the overlay", async () => {
    await mountPanel();
    await profile();
    const panel = q("kb-terminal")!;
    await act(async () => q("kb-term-fullscreen")!.click());
    await settle(2);
    act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="ffff0000" sid="s1" harness="omp" /></QueryClientProvider>));
    await settle(2);
    expect(q("kb-terminal")!.classList.contains("kb-term--max")).toBe(false);
    expect(panel.classList.contains("kb-term--max")).toBe(false);
  });
});

// ------------------------------------------------------- FUTERMPOLISH

describe("W1: the owner chip tells the truth after a grid change", () => {
  it("a list row of another size than the live grid is a stale snapshot: no suffix, never `free`", async () => {
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null }];
    await mountPanel();
    await profile();
    expect(text("kb-term-grid")).toBe("201x48 · free");
    // Another client took 80x24; the list has not caught up.
    act(() => lastSession!.sink.resize(80, 24));
    expect(text("kb-term-grid")).toBe("80x24");
    // The refetch lands: the row now matches and names the owner.
    rows = [{ id: "abcd1234", state: "working", cols: 80, rows: 24, grid_owner: "kb-term" }];
    act(() => root!.render(<QueryClientProvider client={new QueryClient()}><TerminalPanel termId="abcd1234" sid="s1" harness="omp" /></QueryClientProvider>));
    expect(text("kb-term-grid")).toBe("80x24 · web");
  });

  it("a grid-change frame invalidates [\"terms\"] once, debounced, however many frames arrive", async () => {
    const spy = vi.spyOn(QueryClient.prototype, "invalidateQueries");
    await mountPanel();
    await profile();
    spy.mockClear();
    act(() => {
      lastSession!.sink.resize(80, 24);
      lastSession!.sink.resize(100, 30);
      lastSession!.sink.resize(120, 40);
    });
    expect(spy).not.toHaveBeenCalled();
    await new Promise((r) => setTimeout(r, 450));
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy).toHaveBeenCalledWith({ queryKey: ["terms"] });
    spy.mockRestore();
  });

  it("nothing refetches by itself: no resize frame, no invalidation (no polling)", async () => {
    const spy = vi.spyOn(QueryClient.prototype, "invalidateQueries");
    await mountPanel();
    await profile();
    spy.mockClear();
    await new Promise((r) => setTimeout(r, 450));
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });

  it("unmounting cancels the pending refetch", async () => {
    const spy = vi.spyOn(QueryClient.prototype, "invalidateQueries");
    await mountPanel();
    await profile();
    spy.mockClear();
    act(() => lastSession!.sink.resize(80, 24));
    act(() => root!.unmount());
    root = null;
    await new Promise((r) => setTimeout(r, 450));
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});

describe("W3: a grid change recomputes the font from scratch", () => {
  it("201x48 -> 80x24 -> 201x48 in the same box: the font follows the plan each way and Cropped follows the floor", async () => {
    box = { w: 1230, h: 576 };
    await mountPanel();
    const term = await profile();
    expect(term.options.fontSize).toBe(9);
    expect(q("kb-term-cropped")).toBeNull();
    act(() => lastSession!.sink.resize(80, 24));
    await settle(2);
    // byHeight: floor(576 / (24 * 1.32)) = 18; width allows more.
    expect(term.options.fontSize).toBe(18);
    expect(text("kb-term-font")).toBe("font 18px");
    expect(q("kb-term-cropped")).toBeNull();
    act(() => lastSession!.sink.resize(120, 40));
    await settle(2);
    // floor(576 / (40 * 1.32)) = 10 (the fresh-load value)
    expect(term.options.fontSize).toBe(10);
    expect(q("kb-term-cropped")).toBeNull();
    act(() => lastSession!.sink.resize(201, 48));
    await settle(2);
    expect(term.options.fontSize).toBe(9);
  });

  it("a narrow box crops at the floor and a smaller grid in it un-crops", async () => {
    box = { w: 800, h: 800 };
    await mountPanel();
    await profile();
    expect(q("kb-term-cropped")).not.toBeNull();
    act(() => lastSession!.sink.resize(100, 30));
    await settle(2);
    expect(q("kb-term-cropped")).toBeNull();
  });
});

describe("W4: a copy ends the selection", () => {
  it("the second Ctrl+C after a copy is ^C (left to xterm), not another copy", async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await mountPanel();
    const term = await profile();
    term.selection = "picked text";
    expect(term.keyHandler!(keyEv({ ctrlKey: true }))).toBe(false);
    expect(writeText).toHaveBeenCalledTimes(1);
    await settle(3);
    expect(term.selection).toBe("");
    const second = keyEv({ ctrlKey: true });
    expect(term.keyHandler!(second)).toBe(true);
    expect(second.preventDefault).not.toHaveBeenCalled();
    expect(writeText).toHaveBeenCalledTimes(1);
  });

  it("Ctrl+Shift+C also ends the selection", async () => {
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText: vi.fn(async () => {}) } });
    await mountPanel();
    const term = await profile();
    term.selection = "abc";
    term.keyHandler!(keyEv({ ctrlKey: true, shiftKey: true, key: "C" }));
    await settle(3);
    expect(term.selection).toBe("");
  });
});

describe("W2: the panel scrolls itself into view on open and before a Fit, never on a re-render", () => {
  it("scrolls once on mount, not on profile / state / resize frames", async () => {
    const scroll = vi.fn();
    (Element.prototype as unknown as { scrollIntoView: unknown }).scrollIntoView = scroll;
    try {
      await mountPanel();
      expect(scroll).toHaveBeenCalledTimes(1);
      expect(scroll.mock.contexts[0]).toBe(q("kb-terminal"));
      await profile();
      act(() => lastSession!.sink.resize(80, 24));
      act(() => lastSession!.callbacks.onState({ phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null }));
      await settle(2);
      expect(scroll).toHaveBeenCalledTimes(1);
    } finally {
      delete (Element.prototype as unknown as Record<string, unknown>).scrollIntoView;
    }
  });

  it("Fit to window scrolls (instantly) BEFORE it measures and posts", async () => {
    const order: string[] = [];
    (Element.prototype as unknown as { scrollIntoView: unknown }).scrollIntoView = vi.fn(() => order.push("scroll"));
    try {
      await mountPanel();
      await profile();
      order.length = 0;
      rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
      act(() => q("kb-term-fit")!.click());
      await settle(10);
      expect(order).toEqual(["scroll"]);
      expect(rig.urls()).toEqual(["/api/terms/abcd1234/take-size"]);
    } finally {
      delete (Element.prototype as unknown as Record<string, unknown>).scrollIntoView;
    }
  });
});

describe("FUTERMATTN attention hooks", () => {
  async function mountWith(onBell: () => void): Promise<FakeTermInst> {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" onBell={onBell} />
        </QueryClientProvider>,
      ),
    );
    await settle(4);
    return profile();
  }

  it("shows the program's OSC title as text, sanitized and capped", async () => {
    await mountPanel();
    const term = await profile();
    expect(q("kb-term-title")).toBeNull();
    act(() => term.titleCbs.forEach((cb) => cb("<img src=x onerror=alert(1)>\u202e " + "w".repeat(200))));
    const el = q("kb-term-title")!;
    expect(el.querySelector("img")).toBeNull();
    expect(el.textContent!.startsWith("<img src=x onerror=alert(1)>")).toBe(true);
    expect(el.textContent).not.toContain("\u202e");
    expect(el.textContent!.length).toBeLessThanOrEqual(60);
    act(() => term.titleCbs.forEach((cb) => cb("")));
    expect(q("kb-term-title")).toBeNull();
  });

  it("a BEL while the tab is hidden calls onBell; a BEL in a visible, focused terminal does not", async () => {
    const onBell = vi.fn();
    const term = await mountWith(onBell);
    const focusedEl = document.createElement("textarea");
    term.element!.appendChild(focusedEl);
    focusedEl.focus();
    const hf = vi.spyOn(document, "hasFocus").mockReturnValue(true);
    act(() => term.bellCbs.forEach((cb) => cb()));
    hf.mockRestore();
    expect(onBell).not.toHaveBeenCalled();
    Object.defineProperty(document, "hidden", { configurable: true, get: () => true });
    try {
      act(() => term.bellCbs.forEach((cb) => cb()));
      expect(onBell).toHaveBeenCalledTimes(1);
    } finally {
      delete (document as unknown as Record<string, unknown>).hidden;
    }
  });

  it("a BEL while the window lost focus (tab visible, textarea still active) calls onBell", async () => {
    const onBell = vi.fn();
    const term = await mountWith(onBell);
    const focusedEl = document.createElement("textarea");
    term.element!.appendChild(focusedEl);
    focusedEl.focus();
    const spy = vi.spyOn(document, "hasFocus").mockReturnValue(false);
    try {
      act(() => term.bellCbs.forEach((cb) => cb()));
      expect(onBell).toHaveBeenCalledTimes(1);
    } finally {
      spy.mockRestore();
    }
  });

  it("a BEL with focus elsewhere in the page calls onBell", async () => {
    const onBell = vi.fn();
    const term = await mountWith(onBell);
    const other = document.createElement("input");
    document.body.appendChild(other);
    other.focus();
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    try {
      act(() => term.bellCbs.forEach((cb) => cb()));
      expect(onBell).toHaveBeenCalledTimes(1);
    } finally {
      vi.restoreAllMocks();
    }
  });
});

// ------------------------------------------------------------ FUMINPANEL

const LIVE = { phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null };

describe("FUMINPANEL: the panel's wiring to the session machine", () => {
  it("a socket close reaches the session with its code AND reason (the caption's input)", async () => {
    class FakeWs {
      static last: FakeWs | null = null;
      binaryType = "";
      onmessage: ((ev: unknown) => void) | null = null;
      onclose: ((ev: { code: number; reason: string }) => void) | null = null;
      onerror: (() => void) | null = null;
      constructor(public url: string) {
        FakeWs.last = this;
      }
      send(): void {}
      close(): void {}
    }
    vi.stubGlobal("WebSocket", FakeWs);
    await mountPanel();
    const onClose = vi.fn();
    lastSession!.effects.openSocket("wss://x/ws", { onMessageBinary() {}, onMessageText() {}, onClose });
    FakeWs.last!.onclose!({ code: 1001, reason: "idle" });
    expect(onClose).toHaveBeenCalledWith(1001, "idle");
  });

  it("a visibilitychange reaches the session's setHidden (hidden, then visible)", async () => {
    await mountPanel();
    const desc = Object.getOwnPropertyDescriptor(document, "visibilityState");
    try {
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "hidden" });
      document.dispatchEvent(new Event("visibilitychange"));
      expect(setHiddenMock).toHaveBeenLastCalledWith(true);
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => "visible" });
      document.dispatchEvent(new Event("visibilitychange"));
      expect(setHiddenMock).toHaveBeenLastCalledWith(false);
    } finally {
      if (desc) Object.defineProperty(document, "visibilityState", desc);
      else delete (document as unknown as Record<string, unknown>).visibilityState;
    }
  });
});

describe("FUMINPANEL: the program title belongs to one live connection", () => {
  it("is cleared when the session leaves live (reconnecting) and is not resurrected", async () => {
    await mountPanel();
    const term = await profile();
    act(() => lastSession!.callbacks.onState(LIVE));
    act(() => term.titleCbs.forEach((cb) => cb("build running")));
    expect(text("kb-term-title")).toBe("build running");
    act(() => lastSession!.callbacks.onState({ ...LIVE, phase: "reconnecting", caption: "reconnecting in 1s" }));
    expect(q("kb-term-title")).toBeNull();
  });

  it("a title that arrives while live stays through further live state updates", async () => {
    await mountPanel();
    const term = await profile();
    act(() => lastSession!.callbacks.onState(LIVE));
    act(() => term.titleCbs.forEach((cb) => cb("steady")));
    act(() => lastSession!.callbacks.onState({ ...LIVE, applied: 5 }));
    expect(text("kb-term-title")).toBe("steady");
  });
});

describe("FUMINPANEL: copy clears the selection only after the clipboard write succeeded", () => {
  it("keeps the selection while the write is pending, clears it once it resolves", async () => {
    let resolve!: () => void;
    const writeText = vi.fn(() => new Promise<void>((r) => (resolve = r)));
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await mountPanel();
    const term = await profile();
    term.selection = "picked";
    term.keyHandler!(keyEv({ ctrlKey: true }));
    await settle(2);
    expect(writeText).toHaveBeenCalledWith("picked");
    expect(term.selection).toBe("picked");
    resolve();
    await settle(3);
    expect(term.selection).toBe("");
  });

  it("a rejected write keeps the selection and says so (toast.err)", async () => {
    const { toast } = await import("../lib/toast");
    vi.mocked(toast.err).mockClear();
    const writeText = vi.fn(async () => {
      throw new Error("denied");
    });
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    await mountPanel();
    const term = await profile();
    term.selection = "picked";
    term.keyHandler!(keyEv({ ctrlKey: true }));
    await settle(3);
    expect(term.selection).toBe("picked");
    expect(toast.err).toHaveBeenCalledTimes(1);
  });
});

describe("FUMINPANEL: a font or grid change tells the picture layer", () => {
  it("notifies the gfx layer when a fit result changes the font, and not on an unchanged result", async () => {
    await mountPanel();
    await profile();
    expect(gfxMetrics).toHaveBeenCalled();
    gfxMetrics.mockClear();
    // Same grid, same box: a refit keeps the font, nothing moved.
    act(() => roCallbacks.forEach((cb) => cb()));
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    expect(gfxMetrics).not.toHaveBeenCalled();
    // A smaller grid in the same box changes the font: the pictures must follow.
    act(() => lastSession!.sink.resize(80, 24));
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    expect(gfxMetrics).toHaveBeenCalled();
  });
});

describe("FUMINPANEL: the full-screen font bounds", () => {
  it("never goes above 28 in a huge box and never below 8 (then Cropped) in a small one", async () => {
    box = { w: 9000, h: 4000 };
    await mountPanel();
    const term = await profile();
    await act(async () => q("kb-term-fullscreen")!.click());
    await new Promise((r) => setTimeout(r, 150));
    await settle(2);
    expect(term.options.fontSize).toBe(28);
    box = { w: 900, h: 400 };
    act(() => lastSession!.sink.resize(201, 48));
    act(() => roCallbacks.forEach((cb) => cb()));
    await new Promise((r) => setTimeout(r, 400));
    await settle(2);
    expect(term.options.fontSize).toBe(8);
    expect(q("kb-term-cropped")).not.toBeNull();
  });
});

// ------------------------------------------------------------------ BHEAD

describe("BHEAD: the header", () => {
  const status = { model: "opus", context_pct: 42, branch: "feat/x", permission_mode: "plan" };
  const liveRow = (over: Record<string, unknown> = {}) => ({ session_id: "s1", status, ...over });
  let props: Record<string, unknown> = {};

  function renderPanel(extra: Record<string, unknown> = {}): void {
    props = { ...props, ...extra };
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" {...props} />
        </QueryClientProvider>,
      ),
    );
  }
  async function mountWith(extra: Record<string, unknown> = {}): Promise<FakeTermInst> {
    props = {};
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    renderPanel(extra);
    await settle(4);
    return profile();
  }
  const chipIds = (): string[] =>
    Array.from(host!.querySelectorAll<HTMLElement>("[data-testid='kb-term-status-chips'] li")).map((e) => e.dataset.chip!);

  it("title is harness plus the cwd basename; state is a dot and the phase word", async () => {
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null, cwd: "/work/demo" }];
    await mountWith();
    expect(text("kb-term-harness")).toBe("omp");
    expect(text("kb-term-project")).toBe("demo");
    expect(q("kb-term-state")!.dataset.phase).toBe("connecting");
    act(() => lastSession!.callbacks.onState(LIVE));
    expect(q("kb-term-state")!.dataset.phase).toBe("live");
    expect(text("kb-term-state")).toBe("live");
  });

  it("chips render from the bound session's status and vanish when it is absent", async () => {
    liveRows = [liveRow()];
    await mountWith();
    expect(chipIds()).toEqual(["model", "context", "mode", "branch"]);
    expect(text("kb-term-status-chips")).toContain("ctx 42%");
    liveRows = [liveRow({ status: undefined })];
    renderPanel();
    expect(q("kb-term-status-chips")).toBeNull();
    liveRows = [liveRow({ status: { model: "opus" } })];
    renderPanel();
    expect(chipIds()).toEqual(["model"]);
  });

  it("an unbound sid or another session's row shows no chips", async () => {
    liveRows = [liveRow({ session_id: "other" })];
    await mountWith();
    expect(q("kb-term-status-chips")).toBeNull();
    liveRows = [liveRow()];
    renderPanel({ sid: "" });
    expect(q("kb-term-status-chips")).toBeNull();
  });

  it("headerActions sit in their own slot before the pane verbs, in the bar", async () => {
    await mountWith({ headerActions: <button data-testid="x-palette">Cmd</button> });
    const actions = q("kb-term-actions")!;
    expect(actions.querySelector('[data-testid="x-palette"]')).not.toBeNull();
    const bar = q("kb-term-bar")!;
    expect(bar.contains(actions)).toBe(true);
    const find = q("kb-term-find-toggle")!;
    expect(actions.compareDocumentPosition(find) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(actions.compareDocumentPosition(q("kb-term-close")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("no actions slot without headerActions; secondary readouts live in More", async () => {
    await mountWith();
    expect(q("kb-term-actions")).toBeNull();
    const more = q("kb-term-more")!;
    expect(more.contains(q("kb-term-grid"))).toBe(true);
    expect(more.contains(q("kb-term-input"))).toBe(true);
    expect(more.contains(q("kb-term-find-toggle"))).toBe(false);
  });

  it("initialInput is pasted once, without Enter, only after live", async () => {
    const term = await mountWith({ initialInput: "hello\n" });
    expect(term.paste).not.toHaveBeenCalled();
    act(() => lastSession!.callbacks.onState(LIVE));
    expect(term.paste).toHaveBeenCalledTimes(1);
    expect(term.paste).toHaveBeenCalledWith("hello");
  });

  it("initialInput is never re-sent on reconnect, a re-render or a prop-identity change", async () => {
    const term = await mountWith({ initialInput: "hello" });
    act(() => lastSession!.callbacks.onState(LIVE));
    act(() => lastSession!.callbacks.onState({ ...LIVE, phase: "reconnecting", caption: "reconnecting" }));
    act(() => lastSession!.callbacks.onState(LIVE));
    renderPanel();
    renderPanel({ initialInput: "hel" + "lo", headerActions: <i /> });
    expect(term.paste).toHaveBeenCalledTimes(1);
  });

  it("an unmount and a fresh mount of the same terminal does not re-send it either", async () => {
    const term = await mountWith({ initialInput: "once" });
    act(() => lastSession!.callbacks.onState(LIVE));
    expect(term.paste).toHaveBeenCalledTimes(1);
    act(() => root!.unmount());
    host!.remove();
    terms.length = 0;
    const again = await mountWith({ initialInput: "once" });
    act(() => lastSession!.callbacks.onState(LIVE));
    expect(again.paste).not.toHaveBeenCalled();
  });

  it("no initialInput means no paste", async () => {
    const term = await mountWith();
    act(() => lastSession!.callbacks.onState(LIVE));
    expect(term.paste).not.toHaveBeenCalled();
  });
});

describe("BSTATE: the state overlay", () => {
  const ST = { phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null };
  const set = (o: Record<string, unknown>): void => act(() => lastSession!.callbacks.onState({ ...ST, ...o }));
  const overlay = (): HTMLElement | null => q("kb-term-overlay");
  const view = (): HTMLElement => host!.querySelector<HTMLElement>("[data-kb-term]")!;
  const announce = (): string => text("kb-term-announce");

  it("first connect shows the connecting card; live removes the node", async () => {
    await mountPanel();
    expect(overlay()?.dataset.overlay).toBe("connecting");
    expect(overlay()!.textContent).toContain("Connecting");
    expect(q("kb-term-overlay-action")).toBeNull();
    set({});
    expect(overlay()).toBeNull();
  });

  it("reconnecting shows the scrim, the caption and Reconnect now, which kicks the session once", async () => {
    await mountPanel();
    await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting in 2s (offline)" });
    expect(overlay()?.dataset.overlay).toBe("reconnecting");
    expect(text("kb-term-overlay-detail")).toBe("reconnecting in 2s (offline)");
    act(() => q("kb-term-overlay-action")!.click());
    expect(kickMock).toHaveBeenCalledTimes(1);
    // The header keeps the raw phase word.
    expect(text("kb-term-state")).toBe("reconnecting");
  });

  it("a connecting phase after the stream was live is the reconnecting treatment, not the first-connect card", async () => {
    await mountPanel();
    set({});
    set({ phase: "connecting", caption: "connecting" });
    expect(overlay()?.dataset.overlay).toBe("reconnecting");
    expect(q("kb-term-overlay-action")).toBeNull();
  });

  it("reconnecting then live removes the overlay and never focuses the terminal", async () => {
    await mountPanel();
    const term = await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting" });
    set({});
    expect(overlay()).toBeNull();
    expect(term.focus).not.toHaveBeenCalled();
  });

  it("focus that sat on the overlay's button goes back to the terminal when the overlay goes away", async () => {
    await mountPanel();
    const term = await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting" });
    act(() => q("kb-term-overlay-action")!.focus());
    expect(term.focus).not.toHaveBeenCalled();
    set({});
    expect(term.focus).toHaveBeenCalledTimes(1);
  });

  it("ended shows the exit text and Back to terminals calls onClose", async () => {
    const onClose = vi.fn();
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() =>
      root!.render(
        <QueryClientProvider client={new QueryClient()}>
          <TerminalPanel termId="abcd1234" sid="s1" harness="omp" onClose={onClose} />
        </QueryClientProvider>,
      ),
    );
    await settle(4);
    set({});
    set({ phase: "ended", caption: "exited (code 4)", exitCode: 4 });
    expect(overlay()?.dataset.overlay).toBe("ended");
    expect(overlay()!.textContent).toContain("Session ended");
    expect(text("kb-term-overlay-detail")).toBe("exited (code 4)");
    expect(q("kb-term-overlay-action")!.textContent).toBe("Back to terminals");
    act(() => q("kb-term-overlay-action")!.click());
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("blocked shows the reason; sleeping shows no overlay", async () => {
    await mountPanel();
    set({ phase: "blocked", caption: "blocked: width table unavailable", blockedReason: "width table unavailable" });
    expect(text("kb-term-overlay-detail")).toBe("width table unavailable");
    set({ phase: "sleeping", caption: "asleep" });
    expect(overlay()).toBeNull();
  });

  it("one polite announce region, mounted always, with a sentence per phase and Reconnected on recovery", async () => {
    await mountPanel();
    const region = q("kb-term-announce")!;
    expect(host!.querySelectorAll('[data-testid="kb-term-announce"]')).toHaveLength(1);
    expect(region.getAttribute("role")).toBe("status");
    expect(region.getAttribute("aria-live")).toBe("polite");
    expect(announce()).toBe("Connecting");
    set({});
    expect(q("kb-term-announce")).toBe(region);
    expect(announce()).toBe("");
    set({ phase: "reconnecting", caption: "reconnecting" });
    expect(announce()).toBe("Reconnecting");
    set({});
    expect(announce()).toBe("Reconnected");
    set({ phase: "ended", caption: "exited (code 1)", exitCode: 1 });
    expect(announce()).toBe("Session ended, exited (code 1)");
    // The overlay itself carries no live region.
    expect(overlay()!.querySelector("[aria-live]")).toBeNull();
  });

  it("the pane's view is the same element with the same inline style in every phase (no remount, no resize)", async () => {
    await mountPanel();
    await profile();
    set({});
    const el = view();
    const style = el.getAttribute("style");
    for (const o of [
      { phase: "reconnecting", caption: "reconnecting" },
      { phase: "ended", caption: "exited", exitCode: 0 },
      {},
    ]) {
      set(o);
      expect(view()).toBe(el);
      expect(view().getAttribute("style")).toBe(style);
      expect(view().parentElement!.className).toBe("kb-term__pane");
      expect(view().hidden).toBe(false);
    }
  });

  it("the overlay sits inside the pane, beside the view, never inside the xterm host", async () => {
    await mountPanel();
    set({ phase: "reconnecting", caption: "reconnecting" });
    expect(overlay()!.parentElement).toBe(view().parentElement);
    expect(view().contains(overlay())).toBe(false);
  });

  it("the chip drops a stale owner when the stream leaves live", async () => {
    rows = [];
    await mountPanel();
    await profile();
    set({});
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · web");
    set({ phase: "reconnecting", caption: "reconnecting" });
    expect(text("kb-term-grid")).toBe("201x48");
    set({});
    expect(text("kb-term-grid")).toBe("201x48");
  });

  it("the chip drops a hinted owner when the grid size changes", async () => {
    rows = [];
    await mountPanel();
    await profile();
    set({});
    rig.reply(200, { cols: 166, rows: 43, owner: "kb-term" });
    act(() => q("kb-term-fit")!.click());
    await settle(10);
    expect(text("kb-term-grid")).toBe("201x48 · web");
    act(() => lastSession!.sink.resize(100, 30));
    expect(text("kb-term-grid")).toBe("100x30");
  });
});

describe("BA11Y: accessibility", () => {
  const ST = { phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null };
  const set = (o: Record<string, unknown>): void => act(() => lastSession!.callbacks.onState({ ...ST, ...o }));

  it("the pane is a region named by harness and project, stable across phase and title changes", async () => {
    rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null, cwd: "/work/demo" }] as typeof rows;
    await mountPanel();
    const term = await profile();
    const pane = (): HTMLElement => host!.querySelector<HTMLElement>(".kb-term__pane")!;
    expect(pane().getAttribute("role")).toBe("region");
    expect(pane().getAttribute("aria-label")).toBe("omp terminal in demo");
    set({});
    act(() => term.titleCbs.forEach((cb) => cb("build running")));
    set({ phase: "reconnecting", caption: "reconnecting in 3s" });
    expect(pane().getAttribute("aria-label")).toBe("omp terminal in demo");
  });

  it("the More switch flips screenReaderMode on the same Terminal, persists, and a fresh mount starts with it", async () => {
    window.localStorage.removeItem("kb-term-screen-reader");
    await mountPanel();
    const term = await profile();
    set({});
    expect(term.options.screenReaderMode).toBe(false);
    const sw = q("kb-term-screen-reader") as HTMLInputElement;
    const fetchesBefore = rig.calls?.length;
    const resizeSpy = vi.spyOn(term, "resize");
    const sinkBefore = lastSession!.sink ? Object.keys(lastSession!.sink).length : 0;
    act(() => sw.click());
    expect(term.options.screenReaderMode).toBe(true);
    expect(window.localStorage.getItem("kb-term-screen-reader")).toBe("1");
    expect(terms.length).toBe(1);
    act(() => (q("kb-term-screen-reader") as HTMLInputElement).click());
    expect(term.options.screenReaderMode).toBe(false);
    expect(terms.length).toBe(1);
    expect(window.localStorage.getItem("kb-term-screen-reader")).toBe("0");
    expect(rig.calls?.length).toBe(fetchesBefore);
    expect(resizeSpy).not.toHaveBeenCalled();
    expect(lastSession!.sink ? Object.keys(lastSession!.sink).length : 0).toBe(sinkBefore);
    // A fresh mount reads the stored choice.
    act(() => (q("kb-term-screen-reader") as HTMLInputElement).click());
    act(() => root?.unmount());
    host?.remove();
    terms.length = 0;
    await mountPanel();
    const again = await profile();
    expect(again.options.screenReaderMode).toBe(true);
    window.localStorage.removeItem("kb-term-screen-reader");
  });

  it("focus still on the overlay when it unmounts is handed back to the terminal exactly once", async () => {
    await mountPanel();
    const term = await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting" });
    act(() => q("kb-term-overlay-action")!.focus());
    term.focus.mockClear();
    set({});
    expect(term.focus).toHaveBeenCalledTimes(1);
  });

  it("focus that left the overlay to nothing is not handed to the terminal when the overlay unmounts", async () => {
    await mountPanel();
    const term = await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting" });
    const btn = q("kb-term-overlay-action")!;
    act(() => btn.focus());
    act(() => btn.blur());
    expect(document.activeElement === document.body || document.activeElement === null).toBe(true);
    term.focus.mockClear();
    set({});
    expect(term.focus).not.toHaveBeenCalled();
  });

  it("the overlay detail is the session caption, not a transient status", async () => {
    await mountPanel();
    await profile();
    set({});
    set({ phase: "reconnecting", caption: "reconnecting in 4s (offline)" });
    expect(text("kb-term-overlay-detail")).toBe("reconnecting in 4s (offline)");
  });
});
