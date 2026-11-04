// @vitest-environment jsdom
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useCloseRequest, type CloseRequestOptions } from "./useCloseRequest";

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

class FakeCloseWatcher {
  static instances: FakeCloseWatcher[] = [];
  live = true;
  oncancel: ((e: Event) => void) | null = null;
  onclose: ((e: Event) => void) | null = null;
  constructor() {
    FakeCloseWatcher.instances.push(this);
  }
  requestClose({ cancelable = true }: { cancelable?: boolean } = {}) {
    if (!this.live) return;
    if (cancelable) {
      const ev = new Event("cancel", { cancelable: true });
      this.oncancel?.(ev);
      if (ev.defaultPrevented) return;
    }
    this.close();
  }
  close() {
    if (!this.live) return;
    this.onclose?.(new Event("close"));
    this.destroy();
  }
  destroy() {
    this.live = false;
  }
}
const live = () => FakeCloseWatcher.instances.filter((w) => w.live);
const install = () => {
  FakeCloseWatcher.instances = [];
  (window as unknown as { CloseWatcher?: unknown }).CloseWatcher = FakeCloseWatcher;
};

let root: Root | null = null;
let host: HTMLElement | null = null;

function Probe(p: { active: boolean; onClose: () => void; opts?: CloseRequestOptions }) {
  useCloseRequest(p.active, p.onClose, p.opts);
  return null;
}
function render(p: { active: boolean; onClose: () => void; opts?: CloseRequestOptions }) {
  if (!root) {
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
  }
  act(() => root!.render(createElement(Probe, p)));
}

beforeEach(() => install());
afterEach(() => {
  act(() => root?.unmount());
  host?.remove();
  root = host = null;
  delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
  vi.useRealTimers();
  document.body.innerHTML = "";
});

const escOn = (el: Element) => {
  const e = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
  act(() => {
    el.dispatchEvent(e);
  });
  return e;
};

describe("useCloseRequest", () => {
  it("no watcher when inactive", () => {
    render({ active: false, onClose: vi.fn() });
    expect(FakeCloseWatcher.instances.length).toBe(0);
  });

  it("one watcher when active; destroyed on deactivate", () => {
    const onClose = vi.fn();
    render({ active: true, onClose });
    expect(live().length).toBe(1);
    render({ active: false, onClose });
    expect(live().length).toBe(0);
  });

  it("destroyed on unmount", () => {
    render({ active: true, onClose: vi.fn() });
    act(() => root!.unmount());
    expect(live().length).toBe(0);
    root = createRoot(host!);
  });

  it("close event calls the LATEST onClose without recreating", () => {
    const a = vi.fn();
    const b = vi.fn();
    render({ active: true, onClose: a });
    render({ active: true, onClose: b });
    expect(FakeCloseWatcher.instances.length).toBe(1);
    FakeCloseWatcher.instances[0].requestClose();
    expect(a).not.toHaveBeenCalled();
    expect(b).toHaveBeenCalledTimes(1);
  });

  it("no-op without the API", () => {
    delete (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    iframe.focus();
    const onClose = vi.fn();
    expect(() => render({ active: true, onClose })).not.toThrow();
    expect(onClose).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(iframe);
  });

  it("blurs a focused iframe on activation", () => {
    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    iframe.focus();
    expect(document.activeElement).toBe(iframe);
    render({ active: true, onClose: vi.fn() });
    expect(document.activeElement).not.toBe(iframe);
  });

  it("leaves non-iframe focus alone", () => {
    const btn = document.createElement("button");
    document.body.appendChild(btn);
    btn.focus();
    render({ active: true, onClose: vi.fn() });
    expect(document.activeElement).toBe(btn);
  });

  it("ignored Escape cancels the close", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    render({ active: true, onClose, opts: { ignoreEscape: () => true } });
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    escOn(ta);
    FakeCloseWatcher.instances[0].requestClose();
    expect(onClose).not.toHaveBeenCalled();
    expect(FakeCloseWatcher.instances[0].live).toBe(true);
    expect(FakeCloseWatcher.instances.length).toBe(1);
  });

  it("ignored Escape without activation re-arms", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    render({ active: true, onClose, opts: { ignoreEscape: () => true } });
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    escOn(ta);
    FakeCloseWatcher.instances[0].requestClose({ cancelable: false });
    expect(onClose).not.toHaveBeenCalled();
    act(() => {
      vi.runAllTimers();
    });
    expect(live().length).toBe(1);
    expect(FakeCloseWatcher.instances.length).toBe(2);
  });

  it("flag expires", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    render({ active: true, onClose, opts: { ignoreEscape: () => true } });
    const ta = document.createElement("textarea");
    document.body.appendChild(ta);
    escOn(ta);
    act(() => {
      vi.runAllTimers();
    });
    live()[0].requestClose();
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("the keydown is never prevented", () => {
    const seen = vi.fn();
    window.addEventListener("keydown", seen);
    render({ active: true, onClose: vi.fn(), opts: { ignoreEscape: () => true } });
    const e = escOn(document.body);
    window.removeEventListener("keydown", seen);
    expect(e.defaultPrevented).toBe(false);
    expect(seen).toHaveBeenCalledTimes(1);
  });

  it("re-arm is cancelled on deactivate", () => {
    vi.useFakeTimers();
    const onClose = vi.fn();
    render({ active: true, onClose, opts: { ignoreEscape: () => true } });
    escOn(document.body);
    FakeCloseWatcher.instances[0].requestClose({ cancelable: false });
    render({ active: false, onClose });
    act(() => {
      vi.runAllTimers();
    });
    expect(live().length).toBe(0);
  });

  it("non-ignored close calls onClose once", () => {
    const onClose = vi.fn();
    render({ active: true, onClose });
    FakeCloseWatcher.instances[0].requestClose();
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
