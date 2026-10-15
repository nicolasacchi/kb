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
