// @vitest-environment jsdom
// FUPHONELINKS — a still tap on a URL in the Watch grid opens it; a pan, a long
// press, a slow press or a pinch never does; a wrapped URL is confirmed whole
// first. The scroller never writes to the pty (it has no socket at all), so
// "sends nothing" is pinned by the tap not scrolling and not reaching xterm.

import { act, createElement, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { usePhoneScroller, type ScrollTerm } from "./usePhoneScroller";
import { takePressedLine } from "./pressedLine";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
let clock = 0;

// 40 columns x 10 rows of 10 x 10 px cells; the viewport shows buffer lines 2..11.
const COLS = 40;
const ROWS = 10;
const VIEWPORT_Y = 2;
const LINES: Array<[string, boolean]> = [
  ["", false],
  ["", false],
  ["see https://a.dev/x ok", false], // line 2 = screen row 0
  ["plain text", false],
  ["z".repeat(30) + "https://ex", false], // line 4 = screen row 2
  ["ample.com/q rest", true], // line 5 = screen row 3, a wrapped continuation
  ["", false],
];

function fakeTerm(opts: { alt?: boolean } = {}) {
  const element = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  screen.getBoundingClientRect = () =>
    ({ left: 0, top: 0, width: 400, height: 100, right: 400, bottom: 100, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  element.appendChild(screen);
  const active = {
    viewportY: VIEWPORT_Y,
    length: LINES.length,
    getLine: (y: number) => {
      const l = LINES[y];
      return l ? { translateToString: () => l[0], isWrapped: l[1] } : undefined;
    },
  };
  const scrollLines = vi.fn();
  const term = {
    rows: ROWS,
    cols: COLS,
    element,
    options: { fontSize: 12 },
    buffer: { active, normal: opts.alt ? {} : active },
    scrollLines,
  };
  return { term: term as unknown as ScrollTerm, scrollLines };
}

function Harness(p: { term: ScrollTerm; onLongPress: () => void }) {
  const [wrap, setWrap] = useState<HTMLDivElement | null>(null);
  const [vp, setVp] = useState<HTMLDivElement | null>(null);
  usePhoneScroller({ term: p.term, wrap, viewport: vp, onLongPress: p.onLongPress });
  return createElement(
    "div",
    { className: "watch" },
    createElement("div", { ref: setWrap }, createElement("div", { "data-kb-term-viewport": "", ref: setVp })),
  );
}

function mount(term: ScrollTerm, onLongPress = vi.fn()) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(createElement(Harness, { term, onLongPress })));
  return onLongPress;
}

function ptr(type: string, id: number, x: number, y: number) {
  const ev = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperties(ev, {
    pointerId: { value: id },
    clientX: { value: x },
    clientY: { value: y },
    pointerType: { value: "touch" },
  });
  act(() => {
    host!.querySelector("[data-kb-term-viewport]")!.dispatchEvent(ev);
  });
}

/** Pointer coordinates of the middle of a 1-based column on a screen row. */
const px = (x: number) => (x - 1) * 10 + 5;
const py = (row: number) => row * 10 + 5;

function tap(x: number, row: number, ms = 80) {
  ptr("pointerdown", 1, px(x), py(row));
  clock += ms;
  ptr("pointerup", 1, px(x), py(row));
}

const q = (sel: string) => host!.querySelector(sel) as HTMLElement | null;
let open: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  clock = 1000;
  vi.spyOn(performance, "now").mockImplementation(() => clock);
  open = vi.spyOn(window, "open").mockImplementation(() => null);
  takePressedLine();
});

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("FUPHONELINKS tap to open", () => {
  it("a still tap on a url opens it once, with noopener, and does not scroll or reach xterm", () => {
    const f = fakeTerm();
    mount(f.term);
    tap(7, 0);
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith("https://a.dev/x", "_blank", "noopener,noreferrer");
    expect(f.scrollLines).not.toHaveBeenCalled();
    expect(q('[data-testid="kb-term-link-confirm"]')).toBeNull();
  });

  it("a pointercancel on a url (a browser-cancelled gesture) opens nothing and shows no confirm", () => {
    const f = fakeTerm();
    mount(f.term);
    ptr("pointerdown", 1, px(7), py(0));
    clock += 80;
    ptr("pointercancel", 1, px(7), py(0));
    expect(open).not.toHaveBeenCalled();
    expect(q('[data-testid="kb-term-link-confirm"]')).toBeNull();
    tap(35, 2);
    expect(open).not.toHaveBeenCalled();
  });

  it("a tap beside the url, on plain text or on an empty row opens nothing", () => {
    mount(fakeTerm().term);
    tap(2, 0);
    tap(21, 0);
    tap(3, 1);
    tap(3, 9);
    expect(open).not.toHaveBeenCalled();
  });

  it("a pan (10 px or more) never opens", () => {
    const f = fakeTerm();
    mount(f.term);
    ptr("pointerdown", 1, px(7), py(0));
    ptr("pointermove", 1, px(7), py(0) + 12);
    clock += 80;
    ptr("pointerup", 1, px(7), py(0) + 12);
    expect(open).not.toHaveBeenCalled();
    ptr("pointerdown", 1, px(7), py(0));
    ptr("pointermove", 1, px(7) + 12, py(0));
    ptr("pointerup", 1, px(7) + 12, py(0));
    expect(open).not.toHaveBeenCalled();
  });

  it("a press of 550 ms or more is not a tap", () => {
    mount(fakeTerm().term);
    tap(7, 0, 550);
    expect(open).not.toHaveBeenCalled();
  });

  it("a long press opens Read, never the link", () => {
    vi.useFakeTimers();
    const onLong = mount(fakeTerm().term);
    ptr("pointerdown", 1, px(7), py(0));
    act(() => {
      vi.advanceTimersByTime(600);
    });
    clock += 600;
    ptr("pointerup", 1, px(7), py(0));
    expect(onLong).toHaveBeenCalledTimes(1);
    expect(open).not.toHaveBeenCalled();
  });

  it("a touch that stops a coasting fling is not a tap", () => {
    const f = fakeTerm();
    mount(f.term);
    ptr("pointerdown", 1, px(7), py(5));
    clock += 20;
    ptr("pointermove", 1, px(7), py(5) + 30);
    clock += 20;
    ptr("pointermove", 1, px(7), py(5) + 60);
    clock += 10;
    ptr("pointerup", 1, px(7), py(5) + 60);
    // The fling is still coasting (its frame has not run): the next touch stops it.
    tap(7, 0);
    expect(open).not.toHaveBeenCalled();
  });

  it("a pinch never opens a link, and the fingers lifting afterwards do not tap", () => {
    mount(fakeTerm().term);
    ptr("pointerdown", 1, px(7), py(0));
    ptr("pointerdown", 2, px(9), py(0));
    ptr("pointerup", 2, px(9), py(0));
    ptr("pointerup", 1, px(7), py(0));
    expect(open).not.toHaveBeenCalled();
  });

  it("a url wrapped across rows resolves whole and asks first; Open opens it, Cancel does not", () => {
    mount(fakeTerm().term);
    tap(35, 2);
    const row = q('[data-testid="kb-term-link-confirm"]');
    expect(row).not.toBeNull();
    expect(row!.textContent).toContain("https://example.com/q");
    expect(open).not.toHaveBeenCalled();
    act(() => q('[data-testid="kb-term-link-cancel"]')!.click());
    expect(q('[data-testid="kb-term-link-confirm"]')).toBeNull();
    expect(open).not.toHaveBeenCalled();
    // The second row of the same url resolves to the same whole address.
    tap(4, 3);
    expect(q('[data-testid="kb-term-link-confirm"]')!.textContent).toContain("https://example.com/q");
    act(() => q('[data-testid="kb-term-link-open"]')!.click());
    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith("https://example.com/q", "_blank", "noopener,noreferrer");
    expect(q('[data-testid="kb-term-link-confirm"]')).toBeNull();
  });

  it("the confirm row goes away with the scroller", () => {
    mount(fakeTerm().term);
    tap(35, 2);
    expect(q('[data-testid="kb-term-link-confirm"]')).not.toBeNull();
    act(() => root!.unmount());
    expect(document.querySelector('[data-testid="kb-term-link-confirm"]')).toBeNull();
    root = createRoot(host!);
  });
});

describe("FUPHONELINKS pressed row for Read", () => {
  it("a long press remembers the buffer line under the finger", () => {
    vi.useFakeTimers();
    mount(fakeTerm().term);
    ptr("pointerdown", 1, px(3), py(2));
    act(() => {
      vi.advanceTimersByTime(600);
    });
    expect(takePressedLine()).toBe(VIEWPORT_Y + 2);
    expect(takePressedLine()).toBeNull();
  });

  it("in the alternate screen there is no scrollback line to centre on", () => {
    vi.useFakeTimers();
    mount(fakeTerm({ alt: true }).term);
    ptr("pointerdown", 1, px(3), py(2));
    act(() => {
      vi.advanceTimersByTime(600);
    });
    expect(takePressedLine()).toBeNull();
  });
});

// PHIN0 - tap-to-focus for the direct-typing input. The keyboard is raised from the
// `click` the browser synthesizes after a still tap, so the touch handler must not cancel it.
describe("PHIN0 tap to focus", () => {
  function FocusHarness(p: { term: ScrollTerm; onTapFocus?: () => void; focusOn?: "click" | "pointerup" }) {
    const [wrap, setWrap] = useState<HTMLDivElement | null>(null);
    const [vp, setVp] = useState<HTMLDivElement | null>(null);
    usePhoneScroller({
      term: p.term,
      wrap,
      viewport: vp,
      onLongPress: () => {},
      ...(p.onTapFocus ? { onTapFocus: p.onTapFocus } : {}),
      ...(p.focusOn ? { focusOn: p.focusOn } : {}),
    });
    return createElement(
      "div",
      { className: "watch" },
      createElement("div", { ref: setWrap }, createElement("div", { "data-kb-term-viewport": "", ref: setVp })),
    );
  }

  function mountFocus(p: { onTapFocus?: () => void; focusOn?: "click" | "pointerup"; term?: ScrollTerm }) {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    const term = p.term ?? fakeTerm().term;
    act(() => root!.render(createElement(FocusHarness, { term, ...(p.onTapFocus ? { onTapFocus: p.onTapFocus } : {}), ...(p.focusOn ? { focusOn: p.focusOn } : {}) })));
  }

  const click = () =>
    act(() => {
      q("[data-kb-term-viewport]")!.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
    });

  const touch = (type: string) => {
    const ev = new Event(type, { bubbles: true, cancelable: true });
    act(() => {
      q("[data-kb-term-viewport]")!.dispatchEvent(ev);
    });
    return ev;
  };

  it("with onTapFocus the touch sequence is NOT cancelled, so the browser still synthesizes the click", () => {
    mountFocus({ onTapFocus: vi.fn() });
    for (const t of ["touchstart", "touchmove", "touchend"]) expect(touch(t).defaultPrevented).toBe(false);
  });

  it("without onTapFocus the touch sequence is cancelled exactly as before", () => {
    mountFocus({});
    for (const t of ["touchstart", "touchmove", "touchend"]) expect(touch(t).defaultPrevented).toBe(true);
  });

  it("touch events still stop at the viewport (xterm never sees them)", () => {
    mountFocus({ onTapFocus: vi.fn() });
    const above = vi.fn();
    host!.addEventListener("touchend", above);
    touch("touchend");
    expect(above).not.toHaveBeenCalled();
  });

  it("a still tap off any URL focuses from the click, once", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus });
    tap(3, 1);
    expect(focus).not.toHaveBeenCalled(); // nothing at pointerup: the click is the gesture
    clock += 30;
    click();
    expect(focus).toHaveBeenCalledTimes(1);
    click(); // the flag is consumed
    expect(focus).toHaveBeenCalledTimes(1);
  });

  it("a click that comes late, or with no tap before it, focuses nothing", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus });
    click();
    tap(3, 1);
    clock += 500;
    click();
    expect(focus).not.toHaveBeenCalled();
  });

  it("a tap on a URL opens it and does not focus", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus });
    tap(7, 0);
    clock += 30;
    click();
    expect(open).toHaveBeenCalledTimes(1);
    expect(focus).not.toHaveBeenCalled();
  });

  it("a drag never focuses", () => {
    const focus = vi.fn();
    const f = fakeTerm();
    mountFocus({ onTapFocus: focus, term: f.term });
    ptr("pointerdown", 1, px(3), py(1));
    clock += 60;
    ptr("pointermove", 1, px(3), py(1) + 40);
    clock += 60;
    ptr("pointerup", 1, px(3), py(1) + 40);
    clock += 10;
    click();
    expect(focus).not.toHaveBeenCalled();
  });

  it("a long press never focuses", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus });
    ptr("pointerdown", 1, px(3), py(1));
    clock += 700;
    ptr("pointerup", 1, px(3), py(1));
    click();
    expect(focus).not.toHaveBeenCalled();
  });

  it("a new touch disarms an earlier tap's click", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus });
    tap(3, 1);
    ptr("pointerdown", 1, px(3), py(1));
    click();
    expect(focus).not.toHaveBeenCalled();
  });

  it("focusOn pointerup calls at the pointerup and the click adds nothing", () => {
    const focus = vi.fn();
    mountFocus({ onTapFocus: focus, focusOn: "pointerup" });
    tap(3, 1);
    expect(focus).toHaveBeenCalledTimes(1);
    click();
    expect(focus).toHaveBeenCalledTimes(1);
  });
});
