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

  it("reports hidden/visible transitions to the optional hook, hidden before the kick", () => {
    const win = new EventTarget();
    const doc = Object.assign(new EventTarget(), { visibilityState: "hidden" });
    const log: string[] = [];
    bindNetworkKicks(win, doc, () => log.push("kick"), (hidden) => log.push(hidden ? "hidden" : "shown"));
    doc.dispatchEvent(new Event("visibilitychange"));
    doc.visibilityState = "visible";
    doc.dispatchEvent(new Event("visibilitychange"));
    expect(log).toEqual(["hidden", "shown", "kick"]);
  });
});
