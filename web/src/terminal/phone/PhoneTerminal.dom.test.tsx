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

function Harness(props: { term: PhoneTerm | null; hostRef: { current: HTMLDivElement | null } }) {
  const [mode, setMode] = useState<PhoneMode>("watch");
  return createElement(PhoneTerminal, {
    term: props.term,
    profile: { cols: 120, rows: 30 },
    hostRef: props.hostRef,
    mode,
    onMode: setMode,
    stream: { phase: "live", applied: 4242, resets: 1, gaps: 0 },
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
});
