// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Sheet, type SheetProps } from "./Sheet";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

function mount(props: Partial<SheetProps> = {}) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  const p: SheetProps = {
    open: true,
    detent: "peek",
    onDetentChange: vi.fn(),
    onClose: vi.fn(),
    title: "T",
    children: createElement("button", { id: "inner" }, "inner"),
    ...props,
  };
  act(() => root!.render(createElement(Sheet, p)));
  return p;
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  document.body.style.overflow = "";
});

const key = (el: Element, k: string) =>
  act(() => {
    el.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true }));
  });

describe("Sheet", () => {
  it("moves focus into the sheet on open", () => {
    mount();
    const sheet = document.querySelector("[data-kb-sheet]")!;
    expect(sheet.contains(document.activeElement)).toBe(true);
  });

  it("restores focus to the opener on close", () => {
    const opener = document.createElement("button");
    document.body.appendChild(opener);
    opener.focus();
    mount();
    act(() => root!.unmount());
    expect(document.activeElement).toBe(opener);
    root = createRoot(host!);
    opener.remove();
  });

  it("Escape inside the sheet calls onClose", () => {
    const p = mount();
    key(document.getElementById("inner")!, "Escape");
    expect(p.onClose).toHaveBeenCalledTimes(1);
  });

  it("scrim click calls onClose", () => {
    const p = mount();
    act(() => {
      (document.querySelector("[data-kb-scrim]") as HTMLElement).click();
    });
    expect(p.onClose).toHaveBeenCalledTimes(1);
  });

  it("ArrowUp on the handle from peek goes to full; ArrowDown from full goes to peek", () => {
    const p = mount();
    key(document.querySelector('[aria-label="Resize"]')!, "ArrowUp");
    expect(p.onDetentChange).toHaveBeenCalledWith("full");
    act(() => root!.unmount());
    host!.remove();
    const q = mount({ detent: "full" });
    key(document.querySelector('[aria-label="Resize"]')!, "ArrowDown");
    expect(q.onDetentChange).toHaveBeenCalledWith("peek");
  });

  it("ArrowDown from peek closes", () => {
    const p = mount();
    key(document.querySelector('[aria-label="Resize"]')!, "ArrowDown");
    expect(p.onClose).toHaveBeenCalledTimes(1);
    expect(p.onDetentChange).not.toHaveBeenCalled();
  });

  it("exposes dialog semantics and the detent", () => {
    mount({ detent: "full" });
    const sheet = document.querySelector("[data-kb-sheet]")!;
    expect(sheet.getAttribute("role")).toBe("dialog");
    expect(sheet.getAttribute("aria-modal")).toBe("true");
    expect(sheet.getAttribute("data-detent")).toBe("full");
    const lbl = sheet.getAttribute("aria-labelledby")!;
    expect(document.getElementById(lbl)?.textContent).toBe("T");
    expect((sheet as HTMLElement).style.getPropertyValue("--sheet-h")).toBe("92vh");
  });

  it("locks body scroll while open and restores the previous inline value on unmount", () => {
    document.body.style.overflow = "scroll";
    mount();
    expect(document.body.style.overflow).toBe("hidden");
    act(() => root!.unmount());
    expect(document.body.style.overflow).toBe("scroll");
    root = createRoot(host!);
  });

  it("asSheet=false renders no dialog, scrim or lock", () => {
    mount({ asSheet: false });
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(document.querySelector("[data-kb-scrim]")).toBeNull();
    expect(document.getElementById("inner")).not.toBeNull();
    expect(document.body.style.overflow).toBe("");
  });

  it("renders nothing when closed", () => {
    mount({ open: false });
    expect(document.querySelector("[data-kb-sheet]")).toBeNull();
  });

  it("registers a close watcher only while open as a sheet", () => {
    const instances: { live: boolean; onclose: (() => void) | null }[] = [];
    class Fake {
      live = true;
      oncancel = null;
      onclose: (() => void) | null = null;
      constructor() {
        instances.push(this);
      }
      close() {
        this.onclose?.();
        this.live = false;
      }
      destroy() {
        this.live = false;
      }
    }
    (window as unknown as { CloseWatcher?: unknown }).CloseWatcher = Fake;
    try {
      const p = mount();
      expect(instances.filter((w) => w.live).length).toBe(1);
      (instances[0] as unknown as Fake).close();
      expect(p.onClose).toHaveBeenCalledTimes(1);
      act(() => root!.unmount());
      host!.remove();
      instances.length = 0;
      mount({ asSheet: false });
      expect(instances.length).toBe(0);
      act(() => root!.unmount());
      host!.remove();
      mount({ open: false });
      expect(instances.length).toBe(0);
    } finally {
      delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    }
  });

  describe("ignoreEscape (T17)", () => {
    // A close watcher whose requestClose() mimics the platform: a cancelable
    // `cancel` first (the hook prevents it for an ignored Esc), then `close`.
    function installWatcher() {
      const made: { requestClose: () => void }[] = [];
      class Watcher {
        oncancel: ((e: Event) => void) | null = null;
        onclose: ((e: Event) => void) | null = null;
        constructor() {
          made.push(this);
        }
        requestClose() {
          const ev = new Event("cancel", { cancelable: true });
          this.oncancel?.(ev);
          if (ev.defaultPrevented) return;
          this.onclose?.(new Event("close"));
        }
        close() {
          this.onclose?.(new Event("close"));
        }
        destroy() {}
      }
      (window as unknown as { CloseWatcher?: unknown }).CloseWatcher = Watcher;
      return made;
    }
    const uninstall = () => {
      delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    };
    const esc = (el: Element) => {
      const ev = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
      act(() => {
        el.dispatchEvent(ev);
      });
      return ev;
    };

    it("an ignored Escape neither closes through the React handler nor the platform close request", () => {
      const made = installWatcher();
      try {
        const ignoreEscape = vi.fn((e: KeyboardEvent) => e.target instanceof Element && e.target.id === "inner");
        const p = mount({ ignoreEscape });
        const ev = esc(document.getElementById("inner")!);
        expect(ignoreEscape).toHaveBeenCalled();
        expect(ev.defaultPrevented).toBe(false);
        // The platform raises the close request in the keydown's own task.
        act(() => made[made.length - 1].requestClose());
        expect(p.onClose).not.toHaveBeenCalled();
      } finally {
        uninstall();
      }
    });

    it("without the prop Escape still closes", () => {
      const p = mount();
      const ev = esc(document.getElementById("inner")!);
      expect(ev.defaultPrevented).toBe(true);
      expect(p.onClose).toHaveBeenCalledTimes(1);
    });

    it("with the prop and a non-matching target Escape still closes", () => {
      const p = mount({ ignoreEscape: () => false });
      esc(document.getElementById("inner")!);
      expect(p.onClose).toHaveBeenCalledTimes(1);
    });
  });

  it("renders to string under the importing React major", () => {
    const html = renderToString(
      createElement(Sheet, {
        open: true,
        detent: "peek",
        onDetentChange: () => {},
        onClose: () => {},
        title: "S",
        children: "x",
      }),
    );
    expect(html).toContain("data-kb-sheet");
  });
});
