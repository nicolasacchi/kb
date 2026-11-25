// @vitest-environment jsdom
// T17 — the phone view never resizes the grid, never sends a byte, never
// raises the soft keyboard and never owns the terminal's lifetime.

import { act, createElement, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { PhoneTerminal, type PhoneMode, type PhoneTerm } from "./PhoneTerminal";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

const SCREEN_H = 510; // 30 rows x 17 px

interface Fake {
  term: PhoneTerm;
  calls: {
    open: ReturnType<typeof vi.fn>;
    resize: ReturnType<typeof vi.fn>;
    dispose: ReturnType<typeof vi.fn>;
    scrollLines: ReturnType<typeof vi.fn>;
  };
  textarea: HTMLTextAreaElement;
}

function screenEl(): HTMLElement {
  const el = document.createElement("div");
  el.className = "xterm";
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  screen.getBoundingClientRect = () =>
    ({ height: SCREEN_H, width: 867, top: 0, left: 0, right: 867, bottom: SCREEN_H, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  el.appendChild(screen);
  return el;
}

function fakeTerm(opened: boolean): Fake {
  const textarea = document.createElement("textarea");
  const calls = {
    open: vi.fn(),
    resize: vi.fn(),
    dispose: vi.fn(),
    scrollLines: vi.fn(),
  };
  const rows = ["alpha", "beta"];
  const state: { element: HTMLElement | undefined } = { element: opened ? screenEl() : undefined };
  const term = {
    rows: 30,
    cols: 120,
    get element(): HTMLElement | undefined {
      return state.element;
    },
    textarea,
    options: { fontSize: 12, disableStdin: false, scrollback: 20000 },
    buffer: {
      active: { viewportY: 5, baseY: 100, length: 130 },
      normal: {
        length: rows.length,
        getLine: (y: number) => ({ translateToString: () => rows[y] ?? "" }),
      },
    },
    open(parent: HTMLElement) {
      calls.open();
      const el = screenEl();
      state.element = el;
      parent.appendChild(el);
    },
    scrollLines: calls.scrollLines,
    scrollToBottom: vi.fn(),
    onScroll: () => ({ dispose: () => {} }),
    resize: calls.resize,
    dispose: calls.dispose,
  };
  return { term: term as unknown as PhoneTerm, calls, textarea };
}

let extraProps: Record<string, unknown> = {};
let streamOver: Record<string, unknown> = {};

function Harness(props: { term: PhoneTerm | null; hostRef: { current: HTMLDivElement | null } }) {
  const [mode, setMode] = useState<PhoneMode>("watch");
  return createElement(PhoneTerminal, {
    ...extraProps,
    term: props.term,
    profile: { cols: 120, rows: 30 },
    hostRef: props.hostRef,
    mode,
    onMode: setMode,
    stream: { phase: "live", applied: 4242, resets: 1, gaps: 0, ...streamOver },
    caption: "live",
  });
}

function mount(term: PhoneTerm | null) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const hostRef = { current: null as HTMLDivElement | null };
  act(() => root!.render(createElement(Harness, { term, hostRef })));
  return hostRef;
}

afterEach(() => {
  extraProps = {};
  streamOver = {};
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  window.localStorage.clear();
});

const q = (sel: string) => host!.querySelector(sel) as HTMLElement;
const click = (sel: string) =>
  act(() => {
    q(sel).click();
  });

function ptr(type: string, id: number, x: number, y: number) {
  const ev = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperties(ev, {
    pointerId: { value: id },
    clientX: { value: x },
    clientY: { value: y },
    pointerType: { value: "touch" },
  });
  act(() => {
    q("[data-kb-term-viewport]").dispatchEvent(ev);
  });
}

describe("PhoneTerminal", () => {
  it("opens a never-opened terminal exactly once into its host and publishes the test attributes", () => {
    const f = fakeTerm(false);
    const hostRef = mount(f.term);
    expect(f.calls.open).toHaveBeenCalledTimes(1);
    expect(hostRef.current).not.toBeNull();
    expect(f.term.element?.parentElement).toBe(hostRef.current);
    const vp = q("[data-kb-term-viewport]");
    expect(vp.getAttribute("data-kb-term-vy")).toBe("5");
    expect(vp.getAttribute("data-kb-term-base")).toBe("100");
    expect(vp.getAttribute("data-kb-term-len")).toBe("130");
    expect(vp.getAttribute("data-kb-term-font")).toBe("12");
    expect(vp.getAttribute("data-kb-term-cols")).toBe("120");
    expect(vp.getAttribute("data-kb-term-rows")).toBe("30");
    expect(vp.getAttribute("data-kb-term-sl")).toBe("0");
    expect(vp.getAttribute("data-kb-term-offset")).toBe("4242");
    expect(q("[data-kb-term-root]")).not.toBeNull();
  });

  it("re-parents an already-opened terminal without opening it twice", () => {
    const f = fakeTerm(true);
    const old = document.createElement("div");
    old.appendChild(f.term.element as HTMLElement);
    const hostRef = mount(f.term);
    expect(f.calls.open).not.toHaveBeenCalled();
    expect(f.term.element?.parentElement).toBe(hostRef.current);
    expect(old.children).toHaveLength(0);
  });

  it("is read-only: stdin disabled, textarea cannot raise the keyboard", () => {
    const f = fakeTerm(false);
    mount(f.term);
    expect(f.term.options.disableStdin).toBe(true);
    expect(f.textarea.readOnly).toBe(true);
    expect(f.textarea.inputMode).toBe("none");
    expect(f.textarea.tabIndex).toBe(-1);
  });

  it("never resizes the grid: mount, pinch, mode switch and font buttons", () => {
    const f = fakeTerm(false);
    const socket = { send: vi.fn() };
    mount(f.term);
    // Pinch out 1.6x: 12 -> 20.
    ptr("pointerdown", 1, 150, 300);
    ptr("pointerdown", 2, 250, 300);
    ptr("pointermove", 1, 110, 300);
    ptr("pointermove", 2, 270, 300);
    ptr("pointerup", 1, 110, 300);
    ptr("pointerup", 2, 270, 300);
    expect(f.term.options.fontSize).toBe(20);
    expect(q("[data-kb-term-viewport]").getAttribute("data-kb-term-font")).toBe("20");
    expect(window.localStorage.getItem("kb-term-phone-font")).toBe("20");
    click('[data-testid="kb-term-phone-font-up"]');
    expect(f.term.options.fontSize).toBe(22);
    click('[data-testid="kb-term-phone-font-down"]');
    click('[data-testid="kb-term-phone-mode"]');
    expect(q('[data-testid="kb-term-read"]')).not.toBeNull();
    click('[data-testid="kb-term-phone-font-down"]');
    click('[data-testid="kb-term-read-watch"]');
    expect(q('[data-testid="kb-term-read"]')).toBeNull();
    expect(f.calls.resize).not.toHaveBeenCalled();
    expect(socket.send).not.toHaveBeenCalled();
    expect(f.calls.open).toHaveBeenCalledTimes(1);
    // cols/rows are the session grid, untouched by any of it.
    expect(f.term.cols).toBe(120);
    expect(f.term.rows).toBe(30);
  });

  it("a vertical drag of 170 px scrolls exactly 10 lines (the lock distance included)", () => {
    const f = fakeTerm(false);
    mount(f.term);
    ptr("pointerdown", 1, 100, 100);
    ptr("pointermove", 1, 100, 110);
    for (let y = 120; y <= 270; y += 10) ptr("pointermove", 1, 100, y);
    const total = f.calls.scrollLines.mock.calls.reduce((n, c) => n + (c[0] as number), 0);
    expect(total).toBe(-10);
    ptr("pointerup", 1, 100, 270);
  });

  it("Watch stays mounted (hidden) while Read is open, so scroll position survives", () => {
    const f = fakeTerm(false);
    mount(f.term);
    click('[data-testid="kb-term-phone-mode"]');
    const watch = q(".kb-term-phone__watch");
    expect(watch.hidden).toBe(true);
    expect(f.term.element?.isConnected).toBe(true);
    click('[data-testid="kb-term-read-watch"]');
    expect(watch.hidden).toBe(false);
    expect(q("[data-kb-term-viewport]").getAttribute("data-kb-term-vy")).toBe("5");
  });

  it("long press switches to Read; a move past the lock distance cancels it", () => {
    vi.useFakeTimers();
    try {
      const f = fakeTerm(false);
      mount(f.term);
      ptr("pointerdown", 1, 100, 100);
      act(() => {
        vi.advanceTimersByTime(400);
      });
      expect(q('[data-testid="kb-term-read"]')).toBeNull();
      act(() => {
        vi.advanceTimersByTime(150);
      });
      expect(q('[data-testid="kb-term-read"]')).not.toBeNull();
      ptr("pointerup", 1, 100, 100);
      click('[data-testid="kb-term-read-watch"]');
      ptr("pointerdown", 1, 100, 100);
      ptr("pointermove", 1, 100, 112);
      act(() => {
        vi.advanceTimersByTime(700);
      });
      expect(q('[data-testid="kb-term-read"]')).toBeNull();
      ptr("pointerup", 1, 100, 112);
    } finally {
      vi.useRealTimers();
    }
  });

  it("unmount detaches without disposing and hands the terminal back writable", () => {
    const f = fakeTerm(false);
    mount(f.term);
    act(() => root!.unmount());
    expect(f.calls.dispose).not.toHaveBeenCalled();
    expect(f.term.options.disableStdin).toBe(false);
    expect(f.textarea.readOnly).toBe(false);
    root = createRoot(host!);
  });

  it("removes every pointer and touch listener on unmount", () => {
    const f = fakeTerm(false);
    mount(f.term);
    const vp = q("[data-kb-term-viewport]");
    const spy = vi.spyOn(vp, "removeEventListener");
    act(() => root!.unmount());
    const removed = spy.mock.calls.map((c) => c[0]);
    for (const t of ["pointerdown", "pointermove", "pointerup", "pointercancel", "touchstart", "touchmove", "touchend"]) {
      expect(removed).toContain(t);
    }
    root = createRoot(host!);
  });

  it("swallows touch events in the capture phase so xterm never double-scrolls", () => {
    const f = fakeTerm(false);
    mount(f.term);
    const inner = q("[data-kb-term-viewport] .xterm");
    const seenAbove = vi.fn();
    q("[data-kb-term-root]").addEventListener("touchmove", seenAbove);
    const ev = new Event("touchmove", { bubbles: true, cancelable: true });
    act(() => {
      inner.dispatchEvent(ev);
    });
    expect(ev.defaultPrevented).toBe(true);
    expect(seenAbove).not.toHaveBeenCalled();
  });

  describe("full screen button (FUTERMFULL)", () => {
    const setFs = (enabled: boolean | undefined, request: boolean): void => {
      Object.defineProperty(document, "fullscreenEnabled", { configurable: true, value: enabled });
      if (request) {
        (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen = vi.fn(async () => {});
      } else {
        delete (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen;
      }
    };
    afterEach(() => {
      delete (document as unknown as Record<string, unknown>).fullscreenEnabled;
      delete (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen;
    });

    it("is absent where element fullscreen is not available (iPhone Safari, jsdom)", () => {
      setFs(undefined, false);
      mount(fakeTerm(true).term);
      expect(q('[data-testid="kb-term-phone-fullscreen"]')).toBeNull();
    });

    it("is absent when the document disables it, present when enabled and requestFullscreen exists", () => {
      setFs(false, true);
      mount(fakeTerm(true).term);
      expect(q('[data-testid="kb-term-phone-fullscreen"]')).toBeNull();
      act(() => root?.unmount());
      host?.remove();
      setFs(true, true);
      mount(fakeTerm(true).term);
      const btn = q('[data-testid="kb-term-phone-fullscreen"]');
      expect(btn).not.toBeNull();
      // A short label (it wrapped on a narrow phone); the name stays in full for assistive tech.
      expect(btn.textContent).toBe("Full");
      expect(btn.getAttribute("aria-label")).toBe("Full screen");
    });

    it("fullscreens the terminal root (not the page) when tapped", () => {
      setFs(true, true);
      mount(fakeTerm(true).term);
      click('[data-testid="kb-term-phone-fullscreen"]');
      const req = (HTMLElement.prototype as unknown as { requestFullscreen: ReturnType<typeof vi.fn> }).requestFullscreen;
      expect(req).toHaveBeenCalledTimes(1);
      expect(req.mock.contexts[0]).toBe(q("[data-kb-term-root]"));
    });
  });

  describe("FUMINPHONE a failed full screen says so", () => {
    afterEach(() => {
      vi.useRealTimers();
      delete (document as unknown as Record<string, unknown>).fullscreenEnabled;
      delete (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen;
    });

    it("a rejected requestFullscreen shows a caption instead of failing silently, and a later tap clears it", async () => {
      vi.useFakeTimers();
      Object.defineProperty(document, "fullscreenEnabled", { configurable: true, value: true });
      (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen = vi.fn(async () => {
        throw new Error("denied");
      });
      mount(fakeTerm(true).term);
      expect(q('[data-testid="kb-term-phone-fs-note"]')).toBeNull();
      click('[data-testid="kb-term-phone-fullscreen"]');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(2000);
      });
      const note = q('[data-testid="kb-term-phone-fs-note"]');
      expect(note).not.toBeNull();
      expect(note.textContent).toMatch(/full screen/i);
      expect(note.getAttribute("role")).toBe("status");
      click('[data-testid="kb-term-phone-fullscreen"]');
      expect(q('[data-testid="kb-term-phone-fs-note"]')).toBeNull();
    });

    it("no caption when the browser did enter full screen", async () => {
      vi.useFakeTimers();
      Object.defineProperty(document, "fullscreenEnabled", { configurable: true, value: true });
      let fsEl: Element | null = null;
      Object.defineProperty(document, "fullscreenElement", { configurable: true, get: () => fsEl });
      (HTMLElement.prototype as unknown as Record<string, unknown>).requestFullscreen = vi.fn(async function (this: Element) {
        fsEl = this;
        document.dispatchEvent(new Event("fullscreenchange"));
      });
      mount(fakeTerm(true).term);
      click('[data-testid="kb-term-phone-fullscreen"]');
      await act(async () => {
        await vi.advanceTimersByTimeAsync(2000);
      });
      expect(q('[data-testid="kb-term-phone-fs-note"]')).toBeNull();
      fsEl = null;
      delete (document as unknown as Record<string, unknown>).fullscreenElement;
    });
  });

  it("carries ONE visible title: the bar's (the Sheet's is name-only)", () => {
    mount(fakeTerm(true).term);
    expect(q(".kb-term-phone__title").textContent).toBe("Terminal");
  });

  describe("FUTERMPOLISH sheet polish", () => {
    it("W6a: the title row names the harness; the state caption is on its own row, not squeezed by the buttons", () => {
      extraProps = { harness: "claude" };
      mount(fakeTerm(true).term);
      expect(q('[data-testid="kb-term-phone-title"]').textContent).toBe("claude");
      const caption = q('[data-testid="kb-term-caption"]');
      expect(caption.textContent).toBe("live");
      expect(caption.closest(".kb-term-phone__bar")).toBeNull();
      expect(caption.closest(".kb-term-phone__sub")).not.toBeNull();
    });

    it("W6a: the title falls back to Terminal without a harness", () => {
      mount(fakeTerm(true).term);
      expect(q(".kb-term-phone__title").textContent).toBe("Terminal");
    });

    describe("W6e: a grid wider than the screen says so in Watch", () => {
      const sizes = { viewport: 867, wrap: 360 };
      const protoSW = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollWidth");
      const protoCW = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "clientWidth");
      const set = (): void => {
        Object.defineProperty(HTMLElement.prototype, "scrollWidth", {
          configurable: true,
          get(this: HTMLElement) {
            return this.hasAttribute("data-kb-term-viewport") ? sizes.viewport : 0;
          },
        });
        Object.defineProperty(HTMLElement.prototype, "clientWidth", {
          configurable: true,
          get(this: HTMLElement) {
            return this.classList.contains("kb-term-phone__wrap") ? sizes.wrap : 0;
          },
        });
      };
      afterEach(() => {
        if (protoSW) Object.defineProperty(HTMLElement.prototype, "scrollWidth", protoSW);
        else delete (HTMLElement.prototype as unknown as Record<string, unknown>).scrollWidth;
        if (protoCW) Object.defineProperty(HTMLElement.prototype, "clientWidth", protoCW);
        else delete (HTMLElement.prototype as unknown as Record<string, unknown>).clientWidth;
      });

      it("shows the Cropped hint with the way out (pan, or Fit to phone) while the grid is wider than the wrapper", () => {
        sizes.viewport = 867;
        sizes.wrap = 360;
        set();
        mount(fakeTerm(true).term);
        const hint = q('[data-testid="kb-term-phone-cropped"]');
        expect(hint).not.toBeNull();
        expect(hint.textContent).toContain("Cropped");
        expect(hint.textContent).toContain("Fit to phone");
      });

      it("shows no hint when the whole grid fits the screen", () => {
        sizes.viewport = 360;
        sizes.wrap = 360;
        set();
        mount(fakeTerm(true).term);
        expect(q('[data-testid="kb-term-phone-cropped"]')).toBeNull();
      });

      it("shows no hint in Read (the text reflows)", () => {
        sizes.viewport = 867;
        sizes.wrap = 360;
        set();
        mount(fakeTerm(true).term);
        click('[data-testid="kb-term-phone-mode"]');
        expect(q('[data-testid="kb-term-phone-cropped"]')).toBeNull();
      });
    });

    it("W6f: the drive slot is told whether the sheet is in full screen (it folds its controls row then)", () => {
      const seen: boolean[] = [];
      extraProps = {
        renderDrive: (api: { fullscreen: boolean }) => {
          seen.push(api.fullscreen);
          return null;
        },
      };
      mount(fakeTerm(true).term);
      expect(seen.length).toBeGreaterThan(0);
      expect(seen.every((v) => v === false)).toBe(true);
    });
  });
});

// PHIN0 - direct typing behind the hidden pref. Off = the read-only view, no new DOM.
describe("PHIN0 direct typing flag", () => {
  const setSearch = (s: string): void => window.history.replaceState(null, "", `/${s}`);
  afterEach(() => {
    setSearch("");
    window.sessionStorage.clear();
  });
  const proxy = (): HTMLElement | null => host!.querySelector("[data-kb-term-input]");
  const strip = (): HTMLElement | null => host!.querySelector('[data-testid="kb-term-input-debug"]');

  it("flag off: no proxy, no strip, xterm stays read-only, nothing written to storage", () => {
    const send = vi.fn(() => true);
    extraProps = { sendInput: send };
    const f = fakeTerm(false);
    mount(f.term);
    expect(proxy()).toBeNull();
    expect(strip()).toBeNull();
    expect(f.term.options.disableStdin).toBe(true);
    expect(f.textarea.readOnly).toBe(true);
    expect(window.localStorage.getItem("kb.term.phoneDirect")).toBeNull();
    expect(window.localStorage.getItem("kb.term.phoneVariant")).toBeNull();
    expect(send).not.toHaveBeenCalled();
  });

  it("flag off: a still tap + click focuses nothing", () => {
    extraProps = { sendInput: vi.fn(() => true) };
    mount(fakeTerm(false).term);
    ptr("pointerdown", 1, 20, 20);
    ptr("pointerup", 1, 20, 20);
    act(() => {
      q("[data-kb-term-viewport]").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(document.activeElement?.tagName).toBe("BODY");
  });

  it("flag on without a sendInput renders nothing new", () => {
    setSearch("?direct=1");
    mount(fakeTerm(false).term);
    expect(proxy()).toBeNull();
  });

  it("flag on: the proxy lives inside the viewport, xterm stays read-only, unmount removes it", () => {
    setSearch("?direct=1");
    extraProps = { sendInput: vi.fn(() => true) };
    const f = fakeTerm(false);
    mount(f.term);
    const p = proxy();
    expect(p).not.toBeNull();
    expect(q("[data-kb-term-viewport]").contains(p)).toBe(true);
    expect(f.term.options.disableStdin).toBe(true);
    expect(f.textarea.readOnly).toBe(true);
    expect(f.textarea.inputMode).toBe("none");
    expect(window.localStorage.getItem("kb.term.phoneDirect")).toBe("1");
    expect(strip()).toBeNull();
    act(() => root!.unmount());
    expect(document.querySelector("[data-kb-term-input]")).toBeNull();
    root = createRoot(host!);
  });

  it("flag on: a still tap + click focuses the proxy; typing reaches sendInput", () => {
    setSearch("?direct=1");
    const send = vi.fn(() => true);
    extraProps = { sendInput: send };
    mount(fakeTerm(false).term);
    ptr("pointerdown", 1, 20, 20);
    ptr("pointerup", 1, 20, 20);
    expect(document.activeElement).not.toBe(proxy());
    act(() => {
      q("[data-kb-term-viewport]").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    });
    expect(document.activeElement).toBe(proxy());
    const el = proxy() as HTMLTextAreaElement;
    act(() => {
      el.value = "ok";
      el.dispatchEvent(new InputEvent("input", { inputType: "insertText", bubbles: true }));
    });
    expect(send).toHaveBeenCalledWith("ok");
  });

  it("flag on: entering Read takes the proxy (and the keyboard) away", () => {
    setSearch("?direct=1");
    extraProps = { sendInput: vi.fn(() => true) };
    mount(fakeTerm(false).term);
    const el = proxy() as HTMLTextAreaElement;
    act(() => el.focus());
    expect(document.activeElement).toBe(el);
    click('[data-testid="kb-term-phone-mode"]');
    expect(proxy()).toBeNull();
    expect(document.activeElement).not.toBe(el);
    click('[data-testid="kb-term-phone-mode"]');
    expect(proxy()).not.toBeNull();
  });

  it("?inputdebug=1 shows the strip; changing a variant axis remounts the proxy and remembers only the variant", () => {
    setSearch("?direct=1&inputdebug=1");
    const send = vi.fn(() => true);
    extraProps = { sendInput: send };
    mount(fakeTerm(false).term);
    expect(strip()).not.toBeNull();
    const before = proxy();
    const el = before as HTMLTextAreaElement;
    act(() => {
      el.value = "hunter2";
      el.dispatchEvent(new InputEvent("input", { inputType: "insertText", bubbles: true }));
    });
    expect(q('[data-testid="kb-term-input-debug-log"]').textContent).toContain("hunter2");
    const sel = q('[data-testid="kb-term-input-variant-attrs"]') as HTMLSelectElement;
    act(() => {
      sel.value = "input-url";
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    const after = proxy();
    expect(after).not.toBe(before);
    expect(after?.getAttribute("type")).toBe("url");
    expect(JSON.parse(window.localStorage.getItem("kb.term.phoneVariant")!)).toEqual({ send: "every", sentinel: "none", attrs: "input-url" });
    const all = JSON.stringify([...Object.entries(window.localStorage), ...Object.entries(window.sessionStorage)]);
    expect(all).not.toContain("hunter2");
    expect(location.href).not.toContain("hunter2");
  });
});

describe("BSTATE: the phone state overlay", () => {
  it("renders in the watch region while reconnecting, with Reconnect now wired to the prop", () => {
    const onReconnect = vi.fn();
    extraProps = { onReconnect, announce: "Reconnecting" };
    streamOver = { phase: "reconnecting" };
    mount(fakeTerm(true).term);
    const watch = q(".kb-term-phone__watch");
    const overlay = watch.querySelector('[data-testid="kb-term-overlay"]');
    expect(overlay).not.toBeNull();
    expect(watch.querySelector('[data-testid="kb-term-announce"]')!.textContent).toBe("Reconnecting");
    click('[data-testid="kb-term-overlay-action"]');
    expect(onReconnect).toHaveBeenCalledTimes(1);
  });

  it("renders no overlay node while live", () => {
    mount(fakeTerm(true).term);
    expect(host!.querySelector('[data-testid="kb-term-overlay"]')).toBeNull();
    expect(host!.querySelectorAll('[data-testid="kb-term-announce"]')).toHaveLength(1);
  });
});
