// @vitest-environment jsdom
// PHIN1 - cursor-follow in a DOM: one rAF-coalesced frame per burst of triggers,
// reads then one write, the manual-pan suspend, and zero footprint when disabled.

import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createFollowGate } from "./cursorFollow";
import { useCursorFollow, type CursorFollowArgs, type FollowTerm } from "./useCursorFollow";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

// A manual rAF: callbacks run only when the test flushes a frame.
let queue: Array<{ id: number; cb: FrameRequestCallback }> = [];
let nextId = 1;
let rafCalls = 0;
function flushFrame(): number {
  const run = queue;
  queue = [];
  for (const f of run) f.cb(0);
  return run.length;
}

// 100 columns x 20 rows of 10 x 20 px cells, drawn at (0, 100); the Watch box is (0,100)-(400,500).
function geometry() {
  return { cursorX: 5, cursorY: 2, vvHeight: 400, vvTop: 0 };
}
let geo = geometry();

interface Fake {
  term: FollowTerm;
  wrap: HTMLElement;
  viewport: HTMLElement;
  watch: HTMLElement;
  scrollLeftWrites: number[];
  writeParsed: Array<() => void>;
  cursorMove: Array<() => void>;
  disposed: { n: number };
  scrollToBottom: ReturnType<typeof vi.fn>;
  buf: { cursorX: number; cursorY: number; viewportY: number; baseY: number };
}

const rectOf = (l: number, t: number, r: number, b: number) =>
  () => ({ left: l, top: t, right: r, bottom: b, width: r - l, height: b - t, x: l, y: t, toJSON: () => ({}) }) as DOMRect;

function fake(): Fake {
  const element = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  screen.getBoundingClientRect = rectOf(0, 100, 1000, 500);
  element.appendChild(screen);
  const wrap = document.createElement("div");
  wrap.getBoundingClientRect = rectOf(0, 100, 400, 500);
  let sl = 0;
  const scrollLeftWrites: number[] = [];
  Object.defineProperty(wrap, "scrollLeft", {
    configurable: true,
    get: () => sl,
    set: (v: number) => {
      sl = v;
      scrollLeftWrites.push(v);
    },
  });
  const viewport = document.createElement("div");
  const watch = document.createElement("div");
  watch.getBoundingClientRect = rectOf(0, 100, 400, 500);
  const writeParsed: Array<() => void> = [];
  const cursorMove: Array<() => void> = [];
  const disposed = { n: 0 };
  const buf = { cursorX: geo.cursorX, cursorY: geo.cursorY, viewportY: 100, baseY: 100 };
  const scrollToBottom = vi.fn(() => {
    buf.viewportY = buf.baseY;
  });
  const term: FollowTerm = {
    cols: 100,
    rows: 20,
    element,
    buffer: { active: buf },
    scrollToBottom,
    onWriteParsed: (cb) => {
      writeParsed.push(cb);
      return { dispose: () => void (disposed.n += 1) };
    },
    onCursorMove: (cb) => {
      cursorMove.push(cb);
      return { dispose: () => void (disposed.n += 1) };
    },
  };
  return { term, wrap, viewport, watch, scrollLeftWrites, writeParsed, cursorMove, disposed, scrollToBottom, buf };
}

// A fake visual viewport that counts its listeners.
function installVV() {
  const target = new EventTarget();
  const live = new Set<string>();
  const add = target.addEventListener.bind(target);
  const remove = target.removeEventListener.bind(target);
  let added = 0;
  let removed = 0;
  // defineProperties, not Object.assign: assign would read the getters once and freeze them.
  const vv = Object.defineProperties(target, {
    offsetTop: { get: () => geo.vvTop },
    offsetLeft: { value: 0 },
    width: { value: 400 },
    height: { get: () => geo.vvHeight },
  }) as EventTarget & { offsetTop: number; offsetLeft: number; width: number; height: number };
  vv.addEventListener = ((t: string, l: EventListenerOrEventListenerObject, o?: boolean | AddEventListenerOptions) => {
    added += 1;
    live.add(t);
    add(t, l, o);
  }) as typeof vv.addEventListener;
  vv.removeEventListener = ((t: string, l: EventListenerOrEventListenerObject, o?: boolean | EventListenerOptions) => {
    removed += 1;
    remove(t, l, o);
  }) as typeof vv.removeEventListener;
  Object.defineProperty(window, "visualViewport", { configurable: true, value: vv });
  return { vv, counts: () => ({ added, removed }) };
}

function Probe(p: CursorFollowArgs) {
  useCursorFollow(p);
  return null;
}

function mount(f: Fake, extra: Partial<CursorFollowArgs> = {}) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const gate = extra.gate ?? createFollowGate({ now: () => 0 });
  const base: CursorFollowArgs = { term: f.term, wrap: f.wrap, watch: f.watch, enabled: true, gate, ...extra };
  act(() => root!.render(createElement(Probe, base)));
  return {
    rerender: (over: Partial<CursorFollowArgs>) => act(() => root!.render(createElement(Probe, { ...base, ...over }))),
  };
}

beforeEach(() => {
  geo = geometry();
  queue = [];
  nextId = 1;
  rafCalls = 0;
  vi.stubGlobal("requestAnimationFrame", (cb: FrameRequestCallback) => {
    rafCalls += 1;
    const id = nextId++;
    queue.push({ id, cb });
    return id;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    queue = queue.filter((f) => f.id !== id);
  });
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  Reflect.deleteProperty(window, "visualViewport");
  vi.unstubAllGlobals();
});

describe("PHIN1 useCursorFollow: coalescing", () => {
  it("50 resize + scroll + write triggers in one frame run ONE frame callback and ONE write", () => {
    const { vv } = installVV();
    const f = fake();
    f.buf.cursorX = 60; // x = 600..610, past the band's right edge (400)
    mount(f);
    expect(rafCalls).toBe(1); // the enable itself
    for (let i = 0; i < 50; i += 1) {
      vv.dispatchEvent(new Event("resize"));
      vv.dispatchEvent(new Event("scroll"));
      for (const cb of f.writeParsed) cb();
      for (const cb of f.cursorMove) cb();
    }
    expect(rafCalls).toBe(1);
    expect(flushFrame()).toBe(1);
    // 610 + 3 columns of margin over the right edge 400
    expect(f.scrollLeftWrites).toEqual([240]);
  });

  it("a plan of zero writes nothing and schedules nothing further", () => {
    installVV();
    const f = fake(); // cursor at (50, 140): well inside
    mount(f);
    flushFrame();
    expect(f.scrollLeftWrites).toEqual([]);
    expect(f.wrap.style.marginBottom).toBe("");
    expect(queue).toHaveLength(0);
    expect(rafCalls).toBe(1);
  });

  it("a trigger inside a frame schedules at most one more frame", () => {
    installVV();
    const f = fake();
    f.buf.cursorX = 60;
    mount(f);
    f.scrollToBottom.mockImplementation(() => {
      for (const cb of f.writeParsed) cb();
      for (const cb of f.writeParsed) cb();
    });
    f.buf.viewportY = 0; // scrolled up: the frame calls scrollToBottom, which parses
    flushFrame();
    expect(queue).toHaveLength(1);
  });
});

describe("PHIN1 useCursorFollow: the 2 s suspend", () => {
  it("after a manual note nothing is written for 1999 ms and it is written at 2000", () => {
    installVV();
    const clock = { t: 10_000 };
    const gate = createFollowGate({ now: () => clock.t });
    const f = fake();
    f.buf.cursorX = 60;
    mount(f, { gate });
    flushFrame(); // the enable frame follows once
    expect(f.scrollLeftWrites).toHaveLength(1);
    f.wrap.scrollLeft = 0; // the operator pans back by hand
    f.scrollLeftWrites.length = 0;
    gate.noteManual();
    clock.t += 1999;
    for (const cb of f.writeParsed) cb();
    flushFrame();
    expect(f.scrollLeftWrites).toEqual([]);
    clock.t += 1;
    for (const cb of f.writeParsed) cb();
    flushFrame();
    expect(f.scrollLeftWrites).toHaveLength(1);
  });

  it("a frame the gate dropped is re-armed once at the end of the window, with no further trigger", () => {
    vi.useFakeTimers();
    try {
      installVV();
      const clock = { t: 0 };
      const gate = createFollowGate({ now: () => clock.t });
      const f = fake();
      f.buf.cursorX = 60;
      gate.noteManual();
      mount(f, { gate });
      flushFrame(); // blocked: nothing written, a timer is armed
      expect(f.scrollLeftWrites).toEqual([]);
      vi.advanceTimersByTime(1999);
      clock.t += 1999;
      expect(flushFrame()).toBe(0);
      clock.t += 1;
      vi.advanceTimersByTime(1);
      expect(flushFrame()).toBe(1);
      expect(f.scrollLeftWrites).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("a scrolled-up buffer is brought to the bottom only when the gate allows", () => {
    installVV();
    const clock = { t: 0 };
    const gate = createFollowGate({ now: () => clock.t });
    const f = fake();
    mount(f, { gate });
    flushFrame();
    gate.noteManual();
    f.buf.viewportY = 10;
    for (const cb of f.writeParsed) cb();
    flushFrame();
    expect(f.scrollToBottom).not.toHaveBeenCalled();
    expect(f.buf.viewportY).toBe(10);
    clock.t += 2000;
    for (const cb of f.writeParsed) cb();
    flushFrame();
    expect(f.scrollToBottom).toHaveBeenCalledTimes(1);
  });

  it("a buffer already at the bottom is never scrolled", () => {
    installVV();
    const f = fake();
    mount(f);
    flushFrame();
    expect(f.scrollToBottom).not.toHaveBeenCalled();
  });
});

describe("PHIN1 useCursorFollow: vertical follow", () => {
  it("a cursor hidden under the keyboard (Chrome visual-resize) lifts the grid; back in the band it is cleared", () => {
    const { vv } = installVV();
    const f = fake();
    geo.vvHeight = 350; // the visual viewport ends at client y 350
    f.buf.cursorY = 12; // y = 340..360
    mount(f);
    flushFrame();
    // 360 + one row of margin = 380 over the band bottom 350
    expect(f.wrap.style.marginBottom).toBe("30px");
    geo.vvHeight = 800; // keyboard gone: nothing needs lifting any more
    vv.dispatchEvent(new Event("resize"));
    flushFrame();
    expect(f.wrap.style.marginBottom).toBe("");
  });

  it("a cursor clipped at the top (Firefox layout-resize shortens the Watch box) lowers the grid", () => {
    installVV();
    const f = fake();
    f.watch.getBoundingClientRect = rectOf(0, 300, 400, 500); // the Watch box got shorter from the top
    f.buf.cursorY = 0; // y = 100..120, above the box
    mount(f);
    flushFrame();
    // 100 - one row of margin = 80, 220 above the band top 300
    expect(f.wrap.style.marginBottom).toBe("-220px");
  });

  it("works without a visualViewport (the Watch box is the band)", () => {
    const f = fake();
    f.buf.cursorY = 19; // y = 480..500, margin to 520 over 500
    mount(f);
    flushFrame();
    expect(f.wrap.style.marginBottom).toBe("20px");
  });
});

describe("PHIN1 useCursorFollow: lifecycle", () => {
  it("disabled: no listener, no subscription, no frame, no style", () => {
    const { counts } = installVV();
    const f = fake();
    f.buf.cursorX = 60;
    mount(f, { enabled: false });
    expect(counts().added).toBe(0);
    expect(f.writeParsed).toHaveLength(0);
    expect(f.cursorMove).toHaveLength(0);
    expect(rafCalls).toBe(0);
    expect(f.wrap.getAttribute("style")).toBeNull();
    expect(f.scrollLeftWrites).toEqual([]);
  });

  it("enabling subscribes; disabling and unmounting remove everything and clear the offset", () => {
    const { counts } = installVV();
    const f = fake();
    geo.vvHeight = 350;
    f.buf.cursorY = 12;
    const m = mount(f);
    flushFrame();
    expect(counts().added).toBe(2);
    expect(f.writeParsed).toHaveLength(1);
    expect(f.cursorMove).toHaveLength(1);
    expect(f.wrap.style.marginBottom).toBe("30px");
    m.rerender({ enabled: false });
    expect(counts().removed).toBe(2);
    expect(f.disposed.n).toBe(2);
    expect(f.wrap.style.marginBottom).toBe("");
    // a frame that was still pending is cancelled
    m.rerender({ enabled: true });
    expect(queue).toHaveLength(1);
    m.rerender({ enabled: false });
    expect(queue).toHaveLength(0);
    expect(counts().added).toBe(4);
    expect(counts().removed).toBe(4);
  });

  it("a terminal without onCursorMove still follows writes", () => {
    installVV();
    const f = fake();
    delete (f.term as { onCursorMove?: unknown }).onCursorMove;
    f.buf.cursorX = 60;
    mount(f);
    flushFrame();
    expect(f.scrollLeftWrites).toHaveLength(1);
  });
});
