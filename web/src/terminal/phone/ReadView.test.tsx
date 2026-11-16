// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ReadView } from "./ReadView";
import type { ReadBuffer } from "./readText";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

function buffer(rows: string[]): ReadBuffer {
  return {
    length: rows.length,
    getLine: (y) => {
      const t = rows[y];
      return t === undefined ? undefined : { translateToString: () => t };
    },
  };
}

function mount(read: () => ReadBuffer, onWatch = vi.fn()) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(createElement(ReadView, { read, fontSize: 13, cols: 120, onWatch })));
  return onWatch;
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
});

const text = () => host!.querySelector('[data-testid="kb-term-read-text"]')!.textContent;

describe("ReadView", () => {
  it("renders the snapshot text with a caption and the current font", () => {
    mount(() => buffer(["line 00001", "line 00002", ""]));
    expect(text()).toBe("line 00001\nline 00002\n");
    expect(host!.querySelector('[data-testid="kb-term-read-caption"]')!.textContent).toMatch(
      /^Snapshot at \d\d:\d\d:\d\d$/,
    );
    expect((host!.querySelector("pre") as HTMLElement).style.fontSize).toBe("13px");
  });

  it("does not follow the buffer by itself; Refresh re-reads it", () => {
    let rows = ["first"];
    mount(() => buffer(rows));
    expect(text()).toBe("first\n");
    rows = ["first", "second"];
    expect(text()).toBe("first\n");
    act(() => {
      (host!.querySelector('[data-testid="kb-term-read-refresh"]') as HTMLElement).click();
    });
    expect(text()).toBe("first\nsecond\n");
  });

  it("the Watch button calls back", () => {
    const onWatch = mount(() => buffer(["x"]));
    act(() => {
      (host!.querySelector('[data-testid="kb-term-read-watch"]') as HTMLElement).click();
    });
    expect(onWatch).toHaveBeenCalledTimes(1);
  });

  it("shows a placeholder line as one dim [image] marker", () => {
    mount(() => buffer(["before", "\u{10EEEE}\u0301\u{10EEEE}\u0301", "after"]));
    const marker = host!.querySelector(".kb-term-read__image");
    expect(marker?.textContent).toBe("[image]\n");
    expect(text()).toBe("before\n[image]\nafter\n");
    expect(text()).not.toContain("\u{10EEEE}");
  });

  it("starts scrolled to the bottom, again after Refresh", () => {
    mount(() => buffer(["a", "b"]));
    const scroller = host!.querySelector("[data-kb-term-read-scroll]") as HTMLElement;
    // jsdom has no layout: give the scroller a height and record the writes.
    const writes: number[] = [];
    Object.defineProperty(scroller, "scrollHeight", { value: 900, configurable: true });
    Object.defineProperty(scroller, "scrollTop", {
      configurable: true,
      get: () => writes[writes.length - 1] ?? 0,
      set: (v: number) => {
        writes.push(v);
      },
    });
    act(() => {
      (host!.querySelector('[data-testid="kb-term-read-refresh"]') as HTMLElement).click();
    });
    expect(writes).toEqual([900]);
  });
});
