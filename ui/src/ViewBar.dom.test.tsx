// @vitest-environment jsdom
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ViewBar } from "./ViewBar";
import type { Cluster } from "./ViewBar";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;
afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
});

function mount(clusters: Cluster[], maxVisible = 9) {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(<ViewBar clusters={clusters} maxVisible={maxVisible} />));
  return host;
}

describe("ViewBar (jsdom)", () => {
  it("calls onSelect when a control is clicked", () => {
    const onSelect = vi.fn();
    const el = mount([{ label: "A", controls: [{ id: "go", label: "Go", kind: "button", onSelect }] }]);
    act(() => el.querySelector<HTMLButtonElement>("[data-kb-viewbar-control=go]")!.click());
    expect(onSelect).toHaveBeenCalledTimes(1);
  });

  it("passes the value on a select change", () => {
    const onSelect = vi.fn();
    const el = mount([
      {
        label: "A",
        controls: [
          {
            id: "sort",
            label: "Sort",
            kind: "select",
            value: "a",
            options: [
              { value: "a", label: "A" },
              { value: "b", label: "B" },
            ],
            onSelect,
          },
        ],
      },
    ]);
    const sel = el.querySelector<HTMLSelectElement>("select")!;
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!;
      setter.call(sel, "b");
      sel.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(onSelect).toHaveBeenCalledWith("b");
  });

  it("lists overflow in the popover, closes on Escape, and overflow items stay operable", () => {
    const spies = [vi.fn(), vi.fn(), vi.fn()];
    const controls = ["c1", "c2", "c3", "c4", "c5", "c6"].map((id, i) => ({
      id,
      label: `L-${id}`,
      kind: "button" as const,
      key: i >= 3 ? `k${i}` : undefined,
      onSelect: spies[i - 3],
    }));
    const el = mount([{ label: "A", controls }], 3);
    const more = el.querySelector<HTMLButtonElement>("[data-kb-viewbar-more]")!;
    expect(el.querySelector("[role=menu]")).toBeNull();
    act(() => more.click());
    expect(more.getAttribute("aria-expanded")).toBe("true");
    expect(el.querySelectorAll("[role=menu] [role=menuitem]").length).toBe(3);
    expect(el.querySelector("[role=menu]")!.textContent).toContain("k3");
    act(() => {
      document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(el.querySelector("[role=menu]")).toBeNull();
    act(() => more.click());
    act(() => el.querySelectorAll<HTMLButtonElement>("[role=menu] [role=menuitem]")[1].click());
    expect(spies[1]).toHaveBeenCalledTimes(1);
    expect(el.querySelector("[role=menu]")).toBeNull();
  });

  it("closes the popover on an outside pointerdown", () => {
    const controls = ["c1", "c2", "c3"].map((id) => ({ id, label: id, kind: "button" as const }));
    const el = mount([{ label: "A", controls }], 1);
    act(() => el.querySelector<HTMLButtonElement>("[data-kb-viewbar-more]")!.click());
    expect(el.querySelector("[role=menu]")).not.toBeNull();
    act(() => {
      document.body.dispatchEvent(new Event("pointerdown", { bubbles: true }));
    });
    expect(el.querySelector("[role=menu]")).toBeNull();
  });
});
