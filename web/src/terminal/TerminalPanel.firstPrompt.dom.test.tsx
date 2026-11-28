// @vitest-environment jsdom
// HUBPROMPT - the initialInput paste path on the PHONE and the desktop, with the
// xterm engine, the session machine and the picture layer faked. A multi-line
// prompt must never reach the terminal with an Enter-acting newline: not
// through the phone's sendInput (bracketed paste on or off), not through the
// desktop paste.
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
  modes: { bracketedPasteMode: boolean };
}
const terms: FakeTermInst[] = [];
let phoneOn = true;
let bracketedOn = false;

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
    modes = { bracketedPasteMode: bracketedOn };
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
vi.mock("../hooks/useIsMobile", () => ({ useIsMobile: () => phoneOn, usePhonePanel: () => phoneOn }));
vi.mock("./phone/PhoneTerminal", () => ({
  PhoneTerminal: (p: { hostRef: { current: HTMLDivElement | null } }) => <div data-testid="phone-host" ref={p.hostRef} />,
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

import { TerminalPanel } from "./TerminalPanel";
import { resetInitialInput } from "./initialInput";
import { rigFetch, settle } from "./phone/driveTestkit";

// ---------------------------------------------------------------- harness

let host: HTMLElement | null = null;
let root: Root | null = null;
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
  phoneOn = true;
  bracketedOn = false;
  readThemeMock.mockReset();
  readThemeMock.mockReturnValue({ background: "#111111" });
  roCallbacks = [];
  rigFetch();
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

const LIVE = { phase: "live", epoch: null, applied: 0, resets: 0, gaps: 0, attempt: 0, caption: "live", exitCode: null, blockedReason: null };

async function mountLive(initialInput: string): Promise<FakeTermInst> {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() =>
    root!.render(
      <QueryClientProvider client={new QueryClient()}>
        <TerminalPanel termId="abcd1234" sid="s1" harness="omp" initialInput={initialInput} />
      </QueryClientProvider>,
    ),
  );
  await settle(4);
  act(() => lastSession!.callbacks.onProfile(PROFILE));
  await settle(2);
  expect(sendInputMock).not.toHaveBeenCalled();
  act(() => lastSession!.callbacks.onState(LIVE));
  await settle(2);
  return terms[0];
}

describe("HUBPROMPT: initialInput never carries an Enter-acting newline", () => {
  it("phone, bracketed paste OFF: one sendInput, flattened, no CR or LF", async () => {
    phoneOn = true;
    bracketedOn = false;
    await mountLive("line1\nline2");
    expect(sendInputMock).toHaveBeenCalledTimes(1);
    const sent = sendInputMock.mock.calls[0][0] as string;
    expect(sent).toBe("line1 line2");
    expect(/[\r\n]/.test(sent)).toBe(false);
  });

  it("phone, bracketed paste ON: the bracket wraps the flattened text", async () => {
    phoneOn = true;
    bracketedOn = true;
    await mountLive("line1\r\nline2");
    expect(sendInputMock).toHaveBeenCalledTimes(1);
    expect(sendInputMock).toHaveBeenCalledWith("\u001b[200~line1 line2\u001b[201~");
  });

  it("desktop: paste receives the flattened text, once", async () => {
    phoneOn = false;
    const term = await mountLive("line1\nline2\n");
    expect(term.paste).toHaveBeenCalledTimes(1);
    expect(term.paste).toHaveBeenCalledWith("line1 line2");
    expect(sendInputMock).not.toHaveBeenCalled();
  });
});
