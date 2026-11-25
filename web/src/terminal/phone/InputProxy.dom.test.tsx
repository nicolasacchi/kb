// @vitest-environment jsdom
// PHIN0 - the input proxy element and its debug strip in a DOM.

import { act, createElement, type MutableRefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { DEBUG_RING, InputDebugStrip, InputProxy, createDebugLog, type ProxyHandle, type ProxyTerm } from "./InputProxy";
import { DEFAULT_VARIANT, SENTINEL, type DebugEvent, type ProxyVariant } from "./inputProxy";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
});

function fakeTerm(opts: { bracketed?: boolean; cursorX?: number; cursorY?: number } = {}): ProxyTerm {
  const element = document.createElement("div");
  const screen = document.createElement("div");
  screen.className = "xterm-screen";
  // 40 cols x 10 rows of 10 x 20 px cells, drawn at (30, 50).
  screen.getBoundingClientRect = () =>
    ({ left: 30, top: 50, width: 400, height: 200, right: 430, bottom: 250, x: 30, y: 50, toJSON: () => ({}) }) as DOMRect;
  element.appendChild(screen);
  return {
    cols: 40,
    rows: 10,
    element,
    buffer: { active: { cursorX: opts.cursorX ?? 3, cursorY: opts.cursorY ?? 2 } },
    modes: { bracketedPasteMode: opts.bracketed ?? false },
  };
}

function mount(variant: ProxyVariant, send = vi.fn(() => true), term = fakeTerm(), log?: ReturnType<typeof createDebugLog>) {
  host = document.createElement("div");
  const viewport = document.createElement("div");
  viewport.getBoundingClientRect = () =>
    ({ left: 10, top: 40, width: 500, height: 300, right: 510, bottom: 340, x: 10, y: 40, toJSON: () => ({}) }) as DOMRect;
  host.appendChild(viewport);
  document.body.appendChild(host);
  root = createRoot(viewport);
  const handleRef: MutableRefObject<ProxyHandle | null> = { current: null };
  act(() => root!.render(createElement(InputProxy, { term, viewport, variant, send, handleRef, log })));
  const el = viewport.querySelector("[data-kb-term-input]") as HTMLTextAreaElement | HTMLInputElement;
  return { el, send, viewport, handleRef };
}

const typeValue = (el: HTMLTextAreaElement | HTMLInputElement, v: string, inputType = "insertText") => {
  act(() => {
    el.value = v;
    el.dispatchEvent(new InputEvent("input", { inputType, bubbles: true }));
  });
};

describe("the proxy element", () => {
  const cases: Array<[ProxyVariant["attrs"], string, Record<string, string | null>]> = [
    ["textarea", "TEXTAREA", { autocapitalize: "off", autocorrect: "off", spellcheck: "false", autocomplete: "off", enterkeyhint: "enter", inputmode: "text", name: null }],
    ["input-text", "INPUT", { type: "text", autocapitalize: "off", autocorrect: "off", spellcheck: "false", autocomplete: "off", enterkeyhint: "enter", inputmode: "text" }],
    ["input-url", "INPUT", { type: "url", autocapitalize: "off", autocorrect: "off", spellcheck: "false", autocomplete: "off", enterkeyhint: "enter", inputmode: "text" }],
    ["textarea-nofill", "TEXTAREA", { name: "kb-term-proxy", "data-lpignore": "true", "data-1p-ignore": "true", autocapitalize: "off", autocomplete: "off" }],
  ];
  for (const [attrs, tag, want] of cases) {
    it(`attribute set ${attrs}`, () => {
      const { el } = mount({ ...DEFAULT_VARIANT, attrs });
      expect(el.tagName).toBe(tag);
      for (const [k, v] of Object.entries(want)) expect(el.getAttribute(k)).toBe(v);
    });
  }

  it("is present and painted: never display:none, hidden or off-screen", () => {
    const { el } = mount(DEFAULT_VARIANT);
    expect(el.className).toBe("kb-term-input");
    expect(el.style.display).toBe("");
    expect(el.style.visibility).toBe("");
    expect(parseInt(el.style.left, 10)).toBeGreaterThanOrEqual(0);
    expect(parseInt(el.style.top, 10)).toBeGreaterThanOrEqual(0);
  });

  it("sits on the cursor cell, relative to the viewport", () => {
    const { el } = mount(DEFAULT_VARIANT);
    // screen (30,50) - viewport (10,40) = (20,10); cursor (3,2) x cell 10x20
    expect(el.style.left).toBe("50px");
    expect(el.style.top).toBe("50px");
  });

  it("makes a static viewport positioned and gives it back on unmount", () => {
    const { viewport } = mount(DEFAULT_VARIANT);
    expect(viewport.style.position).toBe("relative");
    act(() => root!.unmount());
    root = createRoot(viewport);
    expect(viewport.style.position).toBe("");
  });

  it("zwsp starts holding the sentinel, none starts empty", () => {
    expect(mount({ ...DEFAULT_VARIANT, sentinel: "zwsp" }).el.value).toBe(SENTINEL);
    act(() => root!.unmount());
    host!.remove();
    expect(mount(DEFAULT_VARIANT).el.value).toBe("");
  });

  it("typing goes out as a diff and the element rests empty again", () => {
    const { el, send } = mount(DEFAULT_VARIANT);
    typeValue(el, "hi");
    expect(send).toHaveBeenCalledWith("hi");
    expect(el.value).toBe("");
  });

  it("Enter (beforeinput) is prevented and sent once", () => {
    const { el, send } = mount(DEFAULT_VARIANT);
    const ev = new InputEvent("beforeinput", { inputType: "insertLineBreak", cancelable: true, bubbles: true });
    act(() => {
      el.dispatchEvent(ev);
    });
    expect(ev.defaultPrevented).toBe(true);
    expect(send.mock.calls).toEqual([["\r"]]);
  });

  it("a keydown Enter then its beforeinput is one \\r", () => {
    const { el, send } = mount(DEFAULT_VARIANT);
    const kd = new KeyboardEvent("keydown", { key: "Enter", keyCode: 13, cancelable: true, bubbles: true } as KeyboardEventInit);
    act(() => {
      el.dispatchEvent(kd);
      el.dispatchEvent(new InputEvent("beforeinput", { inputType: "insertLineBreak", cancelable: true, bubbles: true }));
    });
    expect(kd.defaultPrevented).toBe(true);
    expect(send.mock.calls).toEqual([["\r"]]);
  });

  it("paste sends the text only (bracketed only when the terminal asked for it)", () => {
    const clip = (text: string) => {
      const ev = new Event("paste", { cancelable: true, bubbles: true });
      Object.defineProperty(ev, "clipboardData", { value: { getData: () => text } });
      return ev;
    };
    const a = mount(DEFAULT_VARIANT);
    const ev = clip("x\ny");
    act(() => {
      a.el.dispatchEvent(ev);
    });
    expect(ev.defaultPrevented).toBe(true);
    expect(a.send.mock.calls).toEqual([["x\ry"]]);
    act(() => root!.unmount());
    host!.remove();
    const b = mount(DEFAULT_VARIANT, vi.fn(() => true), fakeTerm({ bracketed: true }));
    act(() => {
      b.el.dispatchEvent(clip("z"));
    });
    expect(b.send.mock.calls).toEqual([["\x1b[200~z\x1b[201~"]]);
  });

  it("the handle focuses and blurs the element", () => {
    const { el, handleRef } = mount(DEFAULT_VARIANT);
    act(() => handleRef.current!.focus());
    expect(document.activeElement).toBe(el);
    act(() => handleRef.current!.blur());
    expect(document.activeElement).not.toBe(el);
  });

  it("feeds the debug log when one is given", () => {
    const log = createDebugLog();
    const { el } = mount(DEFAULT_VARIANT, vi.fn(() => true), fakeTerm(), log);
    typeValue(el, "ab");
    expect(log.events().at(-1)).toMatchObject({ type: "input", bytes: "ab", n: 2 });
    expect(log.total()).toBe(2);
  });
});

function ev(i: number, n = 1): DebugEvent {
  return { type: "input", inputType: "insertText", data: `d${i}`, keyCode: 0, composing: false, bytes: `b${i}`, n };
}

describe("the debug strip", () => {
  function strip(log: ReturnType<typeof createDebugLog>, onVariant = vi.fn(), variant = DEFAULT_VARIANT) {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
    act(() => root!.render(createElement(InputDebugStrip, { log, variant, onVariant })));
    return { onVariant };
  }
  const q = (s: string) => host!.querySelector(s) as HTMLElement;

  it("the ring keeps the last 20 events and the counter keeps the whole total", () => {
    const log = createDebugLog();
    strip(log);
    act(() => {
      for (let i = 0; i < DEBUG_RING + 5; i += 1) log.push(ev(i, 2));
    });
    const lines = q('[data-testid="kb-term-input-debug-log"]').textContent!.split("\n");
    expect(lines).toHaveLength(20);
    expect(lines[0]).toContain("d5");
    expect(lines[19]).toContain("d24");
    expect(q('[data-testid="kb-term-input-debug-total"]').textContent).toBe("bytes: 50");
  });

  it("a variant axis change reports the whole new variant", () => {
    const { onVariant } = strip(createDebugLog());
    const sel = q('[data-testid="kb-term-input-variant-send"]') as HTMLSelectElement;
    act(() => {
      sel.value = "deferred";
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(onVariant).toHaveBeenCalledWith({ ...DEFAULT_VARIANT, send: "deferred" });
  });

  it("the buttons keep the keyboard up (pointerdown is prevented)", () => {
    strip(createDebugLog());
    const down = new Event("pointerdown", { cancelable: true, bubbles: true });
    act(() => {
      q('[data-testid="kb-term-input-copy"]').dispatchEvent(down);
    });
    expect(down.defaultPrevented).toBe(true);
  });
});
