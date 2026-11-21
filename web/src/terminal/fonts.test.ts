import { describe, expect, it } from "vitest";
import { FONT_WAIT_MS, TERM_FONT_FAMILY, TERM_FONT_LOAD_SPEC, fontGate } from "./fonts";

describe("TERM_FONT_FAMILY", () => {
  it("leads with the app's mono face and carries symbol fallbacks before the generic family", () => {
    expect(TERM_FONT_FAMILY.startsWith('"JetBrains Mono"')).toBe(true);
    expect(TERM_FONT_FAMILY).toContain("Noto Sans Symbols");
    expect(TERM_FONT_FAMILY.endsWith("monospace")).toBe(true);
  });
});

describe("fontGate", () => {
  /** A manual timer: `fire()` runs the pending callback. */
  function timer() {
    let cb: (() => void) | null = null;
    return {
      set: (fn: () => void) => {
        cb = fn;
        return 1;
      },
      clear: () => {
        cb = null;
      },
      fire: () => cb?.(),
    };
  }

  it("is ready at once when there is no FontFaceSet", async () => {
    const g = fontGate(undefined);
    expect(await g.ready).toBe(true);
    await g.done;
  });

  it("loads the app face and is ready (in time) when it arrives before the bound", async () => {
    const t = timer();
    const loads: string[] = [];
    const g = fontGate({ load: async (spec) => void loads.push(spec) }, FONT_WAIT_MS, t.set, t.clear);
    expect(await g.ready).toBe(true);
    expect(loads).toEqual([TERM_FONT_LOAD_SPEC]);
  });

  it("the bound wins over a slow face, and `done` still fires when it finally loads", async () => {
    const t = timer();
    let release: () => void = () => {};
    const slow = new Promise<void>((r) => {
      release = r;
    });
    const g = fontGate({ load: () => slow }, FONT_WAIT_MS, t.set, t.clear);
    let doneFired = false;
    void g.done.then(() => {
      doneFired = true;
    });
    await Promise.resolve();
    t.fire();
    expect(await g.ready).toBe(false);
    expect(doneFired).toBe(false);
    release();
    await g.done;
    expect(doneFired).toBe(true);
  });

  it("a face that fails to load settles instead of hanging the stream", async () => {
    const g = fontGate({ load: () => Promise.reject(new Error("nope")) }, FONT_WAIT_MS, timer().set, timer().clear);
    expect(await g.ready).toBe(true);
  });
});
