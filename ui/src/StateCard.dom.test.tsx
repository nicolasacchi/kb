// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { StateCard } from "./StateCard";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let host: HTMLElement | null = null;

function mount() {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root!.render(createElement(StateCard, { kind: "empty", title: "T", cli: "kb reindex" })));
}

afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = null;
  host = null;
  vi.unstubAllGlobals();
});

describe("StateCard copy button", () => {
  it("writes the cli to the clipboard when present", () => {
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    mount();
    act(() => host!.querySelector<HTMLButtonElement>(".kb-statecard__copy")!.click());
    expect(writeText).toHaveBeenCalledWith("kb reindex");
  });

  it("is a no-op without a clipboard", () => {
    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
    mount();
    expect(() => act(() => host!.querySelector<HTMLButtonElement>(".kb-statecard__copy")!.click())).not.toThrow();
  });
});
