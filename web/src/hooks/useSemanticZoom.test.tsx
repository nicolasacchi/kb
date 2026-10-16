// @vitest-environment jsdom
import { createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { flushSync } from "react-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useSemanticZoom } from "./useSemanticZoom";
import type { ZoomView } from "../lib/semanticZoom";

function Harness({ view, steps, node }: { view: string; steps: ZoomView[]; node: HTMLElement | null }) {
  useSemanticZoom({ node, view, gesture: true, onStep: (v) => steps.push(v) });
  return null;
}

let root: Root;
let host: HTMLElement;
let content: HTMLElement;
let bar: HTMLElement;
let side: HTMLElement;
const tick = () => new Promise((r) => setTimeout(r, 0));

async function mount(view: string, steps: ZoomView[]) {
  flushSync(() => root.render(createElement(Harness, { view, steps, node: content })));
  await tick();
}

function wheel(target: Element, deltaY: number, ctrlKey = true) {
  const e = new WheelEvent("wheel", { deltaY, ctrlKey, bubbles: true, cancelable: true });
  target.dispatchEvent(e);
  return e;
}

beforeEach(() => {
  const store: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => void (store[k] = v),
    removeItem: (k: string) => void delete store[k],
  });
  host = document.createElement("div");
  content = document.createElement("div");
  bar = document.createElement("div");
  bar.className = "kb-gallery-viewbar";
  side = document.createElement("aside");
  content.append(bar);
  document.body.append(host, content, side);
  root = createRoot(host);
});

afterEach(() => {
  flushSync(() => root.unmount());
  content.remove();
  host.remove();
  side.remove();
  vi.unstubAllGlobals();
});

describe("useSemanticZoom", () => {
  it("steps toward the shelf on ctrl-wheel up and prevents the browser zoom", async () => {
    const steps: ZoomView[] = [];
    await mount("atlas", steps);
    const e = wheel(content, -100);
    expect(steps).toEqual(["list"]);
    expect(e.defaultPrevented).toBe(true);
  });

  it("leaves the event to the browser at the end of the ladder", async () => {
    const steps: ZoomView[] = [];
    await mount("grid", steps);
    const e = wheel(content, -100);
    expect(steps).toEqual([]);
    expect(e.defaultPrevented).toBe(false);
  });

  it("ignores a plain wheel, the ViewBar, and anything outside the content", async () => {
    const steps: ZoomView[] = [];
    await mount("list", steps);
    expect(wheel(content, -100, false).defaultPrevented).toBe(false);
    expect(wheel(bar, -100).defaultPrevented).toBe(false);
    expect(wheel(side, -100).defaultPrevented).toBe(false);
    expect(steps).toEqual([]);
  });

  it("does nothing on a view that is not on the ladder", async () => {
    const steps: ZoomView[] = [];
    await mount("history", steps);
    expect(wheel(content, -100).defaultPrevented).toBe(false);
    expect(steps).toEqual([]);
  });

  it("is off when the preference is off", async () => {
    localStorage.setItem("kb:prefs", JSON.stringify({ pinchZoom: false }));
    const steps: ZoomView[] = [];
    await mount("list", steps);
    expect(wheel(content, -100).defaultPrevented).toBe(false);
    expect(steps).toEqual([]);
  });

  it("- and = step the ladder, but not in a text field or with a modifier", async () => {
    const steps: ZoomView[] = [];
    await mount("list", steps);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "=", bubbles: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "-", bubbles: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "-", ctrlKey: true, bubbles: true }));
    const input = document.createElement("input");
    document.body.append(input);
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "=", bubbles: true }));
    input.remove();
    expect(steps).toEqual(["grid", "atlas"]);
  });

  it("ignores key auto-repeat so holding a key takes one step", async () => {
    const steps: ZoomView[] = [];
    await mount("list", steps);
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "=", bubbles: true }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "=", repeat: true, bubbles: true }));
    expect(steps).toEqual(["grid"]);
  });
});
