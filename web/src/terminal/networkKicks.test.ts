import { describe, expect, it } from "vitest";
import { bindNetworkKicks } from "./networkKicks";

function fakes() {
  const win = new EventTarget();
  const doc = Object.assign(new EventTarget(), { visibilityState: "hidden" });
  let kicks = 0;
  const dispose = bindNetworkKicks(win, doc, () => {
    kicks += 1;
  });
  return { win, doc, dispose, kicks: () => kicks };
}

describe("bindNetworkKicks", () => {
  it("online kicks once", () => {
    const f = fakes();
    f.win.dispatchEvent(new Event("online"));
    expect(f.kicks()).toBe(1);
  });

  it("pageshow kicks once", () => {
    const f = fakes();
    f.win.dispatchEvent(new Event("pageshow"));
    expect(f.kicks()).toBe(1);
  });

  it("visibilitychange kicks only when the tab is visible", () => {
    const f = fakes();
    f.doc.dispatchEvent(new Event("visibilitychange"));
    expect(f.kicks()).toBe(0);
    f.doc.visibilityState = "visible";
    f.doc.dispatchEvent(new Event("visibilitychange"));
    expect(f.kicks()).toBe(1);
  });

  it("the disposer removes all three listeners", () => {
    const f = fakes();
    f.dispose();
    f.doc.visibilityState = "visible";
    f.win.dispatchEvent(new Event("online"));
    f.win.dispatchEvent(new Event("pageshow"));
    f.doc.dispatchEvent(new Event("visibilitychange"));
    expect(f.kicks()).toBe(0);
  });
});
