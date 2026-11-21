import { describe, expect, it, vi } from "vitest";
import { createFullscreen, MAX_CLASS, nativeUsable, phoneFullscreenAvailable, type FsDoc, type FsPanel } from "./fullscreen";

type Listener = () => void;

function rig(opts: { lock?: boolean; request?: "ok" | "reject" | "none"; enabled?: boolean } = {}) {
  const classes = new Set<string>();
  const panel = {
    classList: {
      add: (c: string) => void classes.add(c),
      remove: (c: string) => void classes.delete(c),
      contains: (c: string) => classes.has(c),
    },
  } as FsPanel & { requestFullscreen?: ReturnType<typeof vi.fn> };
  const listeners = new Set<Listener>();
  const doc = {
    fullscreenElement: null as unknown,
    fullscreenEnabled: opts.enabled,
    exitFullscreen: vi.fn(async () => {
      doc.fullscreenElement = null;
    }),
    addEventListener: (_t: string, cb: Listener) => void listeners.add(cb),
    removeEventListener: (_t: string, cb: Listener) => void listeners.delete(cb),
  } as unknown as FsDoc & { exitFullscreen: ReturnType<typeof vi.fn> };
  const fire = (): void => listeners.forEach((cb) => cb());
  if (opts.request !== "none") {
    panel.requestFullscreen = vi.fn(async () => {
      if (opts.request === "reject") throw new Error("denied");
      doc.fullscreenElement = panel;
    });
  }
  const lock = vi.fn(async () => {});
  const unlock = vi.fn();
  const nav = opts.lock === false ? {} : { keyboard: { lock, unlock } };
  const changes: boolean[] = [];
  const ctl = createFullscreen({ panel, doc, nav, onChange: (v) => changes.push(v) });
  return { panel, doc, nav, ctl, lock, unlock, changes, fire, classes, listeners };
}

describe("native full screen (Keyboard Lock present)", () => {
  it("requests fullscreen, locks Escape only after the change event, unlocks on exit", async () => {
    const r = rig();
    await r.ctl.enter();
    expect(r.panel.requestFullscreen).toHaveBeenCalledWith({ navigationUI: "hide" });
    expect(r.classes.has(MAX_CLASS)).toBe(false);
    r.fire();
    expect(r.lock).toHaveBeenCalledWith(["Escape"]);
    expect(r.ctl.active()).toBe(true);
    expect(r.changes.at(-1)).toBe(true);
    await r.ctl.exit();
    expect(r.doc.exitFullscreen).toHaveBeenCalled();
    expect(r.unlock).toHaveBeenCalledTimes(1);
    expect(r.ctl.active()).toBe(false);
    expect(r.changes.at(-1)).toBe(false);
  });

  it("does not lock before the browser reports the element", async () => {
    const r = rig();
    r.panel.requestFullscreen = vi.fn(async () => {});
    await r.ctl.enter();
    expect(r.lock).not.toHaveBeenCalled();
  });

  it("an EXTERNAL exit (the change event with no element) clears the state and unlocks", async () => {
    const r = rig();
    await r.ctl.enter();
    r.fire();
    r.doc.fullscreenElement = null;
    r.fire();
    expect(r.ctl.active()).toBe(false);
    expect(r.unlock).toHaveBeenCalledTimes(1);
    expect(r.changes.at(-1)).toBe(false);
  });

  it("dispose (unmount) exits native fullscreen, unlocks the keyboard and drops the overlay class", async () => {
    const r = rig();
    await r.ctl.enter();
    r.fire();
    r.panel.classList.add(MAX_CLASS);
    r.ctl.dispose();
    expect(r.doc.exitFullscreen).toHaveBeenCalled();
    expect(r.unlock).toHaveBeenCalledTimes(1);
    expect(r.classes.has(MAX_CLASS)).toBe(false);
    expect(r.listeners.size).toBe(0);
  });

  it("a rejected request falls back to the overlay", async () => {
    const r = rig({ request: "reject" });
    await r.ctl.enter();
    expect(r.classes.has(MAX_CLASS)).toBe(true);
    expect(r.ctl.active()).toBe(true);
    expect(r.lock).not.toHaveBeenCalled();
  });
});

describe("overlay (no Keyboard Lock)", () => {
  it("uses the overlay and never calls requestFullscreen", async () => {
    const r = rig({ lock: false });
    await r.ctl.enter();
    expect(r.panel.requestFullscreen).not.toHaveBeenCalled();
    expect(r.classes.has(MAX_CLASS)).toBe(true);
    expect(r.ctl.active()).toBe(true);
    await r.ctl.exit();
    expect(r.classes.has(MAX_CLASS)).toBe(false);
    expect(r.ctl.active()).toBe(false);
  });

  it("a missing requestFullscreen also uses the overlay", async () => {
    const r = rig({ request: "none" });
    await r.ctl.enter();
    expect(r.classes.has(MAX_CLASS)).toBe(true);
  });

  it("dispose drops the overlay class", async () => {
    const r = rig({ lock: false });
    await r.ctl.enter();
    r.ctl.dispose();
    expect(r.classes.has(MAX_CLASS)).toBe(false);
  });
});

describe("phone mode (no overlay, no lock)", () => {
  it("enters natively without locking Escape, and does nothing where the API is missing", async () => {
    const r = rig({ lock: false });
    const classes = new Set<string>();
    const changes: boolean[] = [];
    const ctl = createFullscreen({
      panel: r.panel,
      doc: r.doc,
      nav: r.nav,
      overlay: false,
      lockEscape: false,
      onChange: (v) => changes.push(v),
    });
    await ctl.enter();
    expect(r.panel.requestFullscreen).toHaveBeenCalled();
    expect(r.lock).not.toHaveBeenCalled();
    expect(ctl.active()).toBe(true);
    ctl.dispose();
    const none = rig({ request: "none" });
    const c2 = createFullscreen({ panel: none.panel, doc: none.doc, overlay: false, lockEscape: false, onChange: () => {} });
    await c2.enter();
    expect(c2.active()).toBe(false);
    expect(classes.size).toBe(0);
    expect(none.classes.has(MAX_CLASS)).toBe(false);
  });
});

describe("feature detection", () => {
  it("native needs Keyboard Lock, an enabled document and requestFullscreen", () => {
    const p = { classList: { add() {}, remove() {}, contains: () => false }, requestFullscreen: async () => {} } as FsPanel;
    expect(nativeUsable({ keyboard: { lock: async () => {} } }, {}, p)).toBe(true);
    expect(nativeUsable({}, {}, p)).toBe(false);
    expect(nativeUsable(undefined, {}, p)).toBe(false);
    expect(nativeUsable({ keyboard: { lock: async () => {} } }, { fullscreenEnabled: false }, p)).toBe(false);
    expect(nativeUsable({ keyboard: { lock: async () => {} } }, {}, { ...p, requestFullscreen: undefined })).toBe(false);
  });

  it("the phone button needs fullscreenEnabled === true and requestFullscreen on the element", () => {
    const el = { requestFullscreen: async () => {} };
    expect(phoneFullscreenAvailable({ fullscreenEnabled: true, documentElement: el })).toBe(true);
    expect(phoneFullscreenAvailable({ fullscreenEnabled: false, documentElement: el })).toBe(false);
    expect(phoneFullscreenAvailable({ documentElement: el })).toBe(false);
    expect(phoneFullscreenAvailable({ fullscreenEnabled: true, documentElement: {} })).toBe(false);
  });
});
