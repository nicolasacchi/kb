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
  keyHandler?: (ev: Record<string, unknown>) => boolean;
  onDataCb?: (d: string) => void;
  linkProvider?: { provideLinks(y: number, cb: (l?: Array<Record<string, unknown>>) => void): void };
  scrollCbs: Array<() => void>;
  selection: string;
  buf: { viewportY: number; baseY: number; length: number; lines: Array<{ text: string; wrapped: boolean }> };
  scrollToBottom: ReturnType<typeof vi.fn>;
  focus: ReturnType<typeof vi.fn>;
  select: ReturnType<typeof vi.fn>;
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
    selection = "";
    buf = { viewportY: 0, baseY: 0, length: 1, lines: [] as Array<{ text: string; wrapped: boolean }> };
    scrollToBottom = vi.fn();
    focus = vi.fn();
    select = vi.fn();
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
    scrollLines(): void {}
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
  sink: { resize(cols: number, rows: number): void };
}
let lastSession: SessionOpts | null = null;
const sessionStart = vi.fn();
const sendInputMock = vi.fn();
vi.mock("./session", () => ({
  TermSession: class {
    constructor(o: SessionOpts) {
      lastSession = o;
    }
    start = sessionStart;
    dispose(): void {}
    kick(): void {}
    sendInput(d: string): boolean {
      sendInputMock(d);
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
  sendInputMock.mockReset();
  setGfxTheme.mockReset();
  confirmMock.mockClear();
  gate = { ready: Promise.resolve(true), done: Promise.resolve() };
  box = { w: 1400, h: 800 };
  probe.cellW = 0.6;
  probe.cellH = 1.32;
  rows = [{ id: "abcd1234", state: "working", cols: 201, rows: 48, grid_owner: null }];
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
