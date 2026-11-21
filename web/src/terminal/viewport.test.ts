// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { scrollPanelIntoView } from "./viewport";

afterEach(() => {
  vi.unstubAllGlobals();
});

function el(): { node: HTMLElement; spy: ReturnType<typeof vi.fn> } {
  const node = document.createElement("div");
  const spy = vi.fn();
  (node as unknown as { scrollIntoView: unknown }).scrollIntoView = spy;
  return { node, spy };
}

describe("scrollPanelIntoView", () => {
  it("brings the panel's top to the top of its scroller, animated unless the reader prefers reduced motion", () => {
    vi.stubGlobal("matchMedia", () => ({ matches: false }));
    const a = el();
    expect(scrollPanelIntoView(a.node)).toBe(true);
    expect(a.spy).toHaveBeenCalledWith({ block: "start", inline: "nearest", behavior: "smooth" });
    vi.stubGlobal("matchMedia", () => ({ matches: true }));
    const b = el();
    scrollPanelIntoView(b.node);
    expect(b.spy).toHaveBeenCalledWith({ block: "start", inline: "nearest", behavior: "auto" });
  });

  it("instant for a Fit, which measures straight after", () => {
    vi.stubGlobal("matchMedia", () => ({ matches: false }));
    const a = el();
    scrollPanelIntoView(a.node, { instant: true });
    expect(a.spy).toHaveBeenCalledWith({ block: "start", inline: "nearest", behavior: "auto" });
  });

  it("does nothing without an element or without scrollIntoView (jsdom, a closed subtree)", () => {
    expect(scrollPanelIntoView(null)).toBe(false);
    const bare = document.createElement("div");
    (bare as unknown as { scrollIntoView?: unknown }).scrollIntoView = undefined;
    expect(scrollPanelIntoView(bare)).toBe(false);
  });
});
