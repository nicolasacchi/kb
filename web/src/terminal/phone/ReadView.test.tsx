// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ReadView } from "./ReadView";
import type { ReadBuffer } from "./readText";
import { toast } from "../../lib/toast";
import { setPressedLine } from "./pressedLine";

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

describe("FUPHONELINKS ReadView links, copy and centring", () => {
  it("renders an http(s) url as an anchor that opens in a new tab without opener or referrer", () => {
    mount(() => buffer(["visit https://a.dev/x. now", "ftp://nope javascript:alert(1)"]));
    const a = host!.querySelectorAll("a");
    expect(a).toHaveLength(1);
    expect(a[0]!.getAttribute("href")).toBe("https://a.dev/x");
    expect(a[0]!.textContent).toBe("https://a.dev/x");
    expect(a[0]!.getAttribute("target")).toBe("_blank");
    expect(a[0]!.getAttribute("rel")).toBe("noopener noreferrer");
    // The selectable text is unchanged by the linkification.
    expect(text()).toBe("visit https://a.dev/x. now\nftp://nope javascript:alert(1)\n");
  });

  it("Copy all writes the text with wrapped rows joined, and says so", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    const rows: Array<[string, boolean]> = [
      ["see https://exam", false],
      ["ple.com/p", true],
      ["next", false],
    ];
    mount(() => ({
      length: rows.length,
      getLine: (y) => {
        const r = rows[y];
        return r ? { isWrapped: r[1], translateToString: () => r[0] } : undefined;
      },
    }));
    await act(async () => {
      (host!.querySelector('[data-testid="kb-term-read-copy"]') as HTMLElement).click();
    });
    expect(writeText).toHaveBeenCalledWith("see https://example.com/p\nnext");
    expect(host!.querySelector('[data-testid="kb-term-read-copy"]')!.textContent).toBe("Copied");
  });

  describe("Copy all failures are shown, never reported as Copied", () => {
    const click = async () => {
      await act(async () => {
        (host!.querySelector('[data-testid="kb-term-read-copy"]') as HTMLElement).click();
      });
    };
    const label = () => host!.querySelector('[data-testid="kb-term-read-copy"]')!.textContent;

    it("a rejected writeText toasts an error and the button stays Copy all", async () => {
      const writeText = vi.fn(async () => {
        throw new Error("denied");
      });
      Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
      const err = vi.spyOn(toast, "err").mockImplementation(() => 0);
      mount(() => buffer(["abc"]));
      await click();
      expect(err).toHaveBeenCalledTimes(1);
      expect(label()).toBe("Copy all");
      err.mockRestore();
    });

    it("no clipboard at all (insecure context) toasts an error too", async () => {
      Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
      const err = vi.spyOn(toast, "err").mockImplementation(() => 0);
      mount(() => buffer(["abc"]));
      await click();
      expect(err).toHaveBeenCalledTimes(1);
      expect(label()).toBe("Copy all");
      err.mockRestore();
    });
  });

  describe("centred on the pressed row", () => {
    const stores = new WeakMap<object, number>();
    afterEach(() => {
      for (const k of ["scrollHeight", "clientHeight", "scrollTop"]) {
        delete (HTMLElement.prototype as unknown as Record<string, unknown>)[k];
      }
      setPressedLine(null);
    });
    function layout(): void {
      Object.defineProperty(HTMLElement.prototype, "scrollHeight", { configurable: true, get: () => 2000 });
      Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 200 });
      Object.defineProperty(HTMLElement.prototype, "scrollTop", {
        configurable: true,
        get(this: object) {
          return stores.get(this) ?? 0;
        },
        set(this: object, v: number) {
          stores.set(this, v);
        },
      });
    }
    const hundred = () => Array.from({ length: 100 }, (_, i) => `row ${i}`);

    it("a long press on row 50 places row 50 at the vertical centre", () => {
      layout();
      setPressedLine(50);
      mount(() => buffer(hundred()));
      const scroller = host!.querySelector("[data-kb-term-read-scroll]") as HTMLElement;
      expect(scroller.scrollTop).toBe(50 * 20 + 10 - 100);
    });

    it("without a pressed row it starts at the newest line, and Refresh returns to the newest line", () => {
      layout();
      mount(() => buffer(hundred()));
      const scroller = host!.querySelector("[data-kb-term-read-scroll]") as HTMLElement;
      expect(scroller.scrollTop).toBe(2000);
    });

    it("the pressed row is used once: Refresh goes to the bottom", () => {
      layout();
      setPressedLine(50);
      mount(() => buffer(hundred()));
      act(() => {
        (host!.querySelector('[data-testid="kb-term-read-refresh"]') as HTMLElement).click();
      });
      const scroller = host!.querySelector("[data-kb-term-read-scroll]") as HTMLElement;
      expect(scroller.scrollTop).toBe(2000);
    });
  });
});
