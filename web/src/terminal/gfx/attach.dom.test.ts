// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";
import { attachGfx, TAP_MAX_MS, type GfxHandle, type OpenPicture } from "./attach";
import { FakeTerm } from "./fakeTerm";
import type { ImageDefFrame } from "../wire";

const ID = 0x010203;
const sha = (n: number): string => n.toString(16).padStart(64, "0");

function frame(id: number, over: Partial<ImageDefFrame> = {}): ImageDefFrame {
  return {
    t: "imagedef", seq: 1, id, gen: 1, sha256: sha(id), mime: "image/png",
    w: 400, h: 200, cols: 4, rows: 3, offset: 0, ...over,
  };
}

const tick = (): Promise<void> => new Promise((r) => setTimeout(r, 0));

function setup() {
  const fake = new FakeTerm(20, 10);
  const fetched: string[] = [];
  const frames: Array<() => void> = [];
  const opened: OpenPicture[] = [];
  let clock = 1000;
  const handle: GfxHandle = attachGfx(fake.asTerm() as unknown as Terminal, {
    compact: false,
    bg: "#000000",
    onOpenPicture: (p) => opened.push(p),
    fetchImpl: ((input: RequestInfo | URL) => {
      fetched.push(String(input).split("/").pop() ?? "");
      return Promise.resolve(new Response(new Uint8Array(4), { status: 200 }));
    }) as typeof fetch,
    mediaOptions: {
      createObjectURL: () => "blob:test/pic",
      revokeObjectURL: () => {},
      decode: () => Promise.resolve(),
      sleep: () => Promise.resolve(),
    },
    measure: () => ({ cw: 10, ch: 20, dpr: 1, viewportY: fake.viewportY }),
    now: () => clock,
    requestFrame: (cb) => frames.push(cb),
    cancelFrame: () => {},
  });
  const flushFrames = (): void => {
    for (const cb of frames.splice(0)) cb();
  };
  return { fake, handle, fetched, opened, flushFrames, advance: (ms: number) => (clock += ms) };
}

function stubRects(rect: { left: number; top: number; right: number; bottom: number }) {
  return vi
    .spyOn(HTMLElement.prototype, "getBoundingClientRect")
    .mockImplementation(function (this: HTMLElement) {
      const on = this.classList.contains("kbgfx-clip");
      return (on ? { ...rect, width: rect.right - rect.left, height: rect.bottom - rect.top, x: rect.left, y: rect.top } : { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0, x: 0, y: 0 }) as DOMRect;
    });
}

function pointer(el: Element, type: "pointerdown" | "pointerup", x: number, y: number): void {
  el.dispatchEvent(new MouseEvent(type, { clientX: x, clientY: y, button: 0, bubbles: true }));
}

async function loadedPicture(s: ReturnType<typeof setup>): Promise<void> {
  s.handle.markLive();
  s.handle.onImageDef(frame(ID));
  await tick();
  s.fake.picture(ID, 1, 2, 4, 3);
  s.handle.renderer.render();
}

afterEach(() => {
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

describe("attachGfx tap to open", () => {
  it("tap opens the lightbox on a picture", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    pointer(s.fake.screen, "pointerdown", 30, 30);
    s.advance(100);
    pointer(s.fake.screen, "pointerup", 32, 31);
    expect(s.opened).toEqual([{ id: ID, sha: sha(ID), mime: "image/png", w: 400, h: 200 }]);
  });

  it("a tap outside every picture does nothing", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    pointer(s.fake.screen, "pointerdown", 150, 150);
    pointer(s.fake.screen, "pointerup", 150, 150);
    expect(s.opened).toEqual([]);
  });

  it("drag does not", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    pointer(s.fake.screen, "pointerdown", 30, 30);
    s.advance(100);
    pointer(s.fake.screen, "pointerup", 45, 30);
    expect(s.opened).toEqual([]);
  });

  it("long press does not", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    pointer(s.fake.screen, "pointerdown", 30, 30);
    s.advance(TAP_MAX_MS + 1);
    pointer(s.fake.screen, "pointerup", 30, 30);
    expect(s.opened).toEqual([]);
  });

  it("selection active does not", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    s.fake.selection = true;
    pointer(s.fake.screen, "pointerdown", 30, 30);
    s.advance(100);
    pointer(s.fake.screen, "pointerup", 30, 30);
    expect(s.opened).toEqual([]);
  });

  it("dispose stops tap handling", async () => {
    stubRects({ left: 20, top: 20, right: 60, bottom: 80 });
    const s = setup();
    await loadedPicture(s);
    s.handle.dispose();
    pointer(s.fake.screen, "pointerdown", 30, 30);
    pointer(s.fake.screen, "pointerup", 30, 30);
    expect(s.opened).toEqual([]);
    expect(s.fake.listenerCount).toBe(0);
  });
});

describe("attachGfx store lifecycle", () => {
  it("ResumeFrom path does not clear the store", async () => {
    const s = setup();
    s.handle.onImageDef(frame(1));
    s.handle.onImageDef(frame(2));
    s.handle.markLive();
    // A resume: gfxreset frames and retransmits arrive, onFullJoin does NOT.
    s.handle.onGfxReset(3);
    s.handle.onImageDef(frame(1));
    expect(s.handle.defs.size).toBe(2);
    expect(s.handle.defs.get(1)?.version).toBe(1);
    await tick();
    // The identical retransmit cost nothing: no fetch was started for it.
    expect(s.fetched).toEqual([]);
  });

  it("a full join clears the store and the media cache", async () => {
    const s = setup();
    s.handle.markLive();
    s.handle.onImageDef(frame(1));
    await tick();
    expect(s.handle.media.size).toBe(1);
    s.handle.onFullJoin();
    expect(s.handle.defs.size).toBe(0);
    expect(s.handle.media.size).toBe(0);
  });

  it("live defs load at once, seed-batch defs only when their cells are in the window", async () => {
    const s = setup();
    // Seed batch (before markLive): nothing loads yet.
    s.handle.onImageDef(frame(1));
    s.handle.onImageDef(frame(2));
    s.flushFrames();
    await tick();
    expect(s.fetched).toEqual([]);
    // Id 1 has cells on screen; id 2 has none anywhere.
    s.fake.picture(1, 1, 0, 2, 2);
    s.handle.markLive();
    s.flushFrames();
    await tick();
    expect(s.fetched).toEqual([sha(1)]);
    // A def arriving after the seed loads immediately, with no cells at all.
    s.handle.onImageDef(frame(3));
    await tick();
    expect(s.fetched).toEqual([sha(1), sha(3)]);
    // Scrolling id 2's rows into the window loads it.
    s.fake.picture(2, 40, 0, 2, 2);
    s.fake.baseY = 40;
    s.fake.viewportY = 38;
    s.fake.fire("scroll");
    s.flushFrames();
    await tick();
    expect(s.fetched).toEqual([sha(1), sha(3), sha(2)]);
  });

  it("a full join re-announces and refetches", async () => {
    const s = setup();
    s.handle.markLive();
    s.handle.onImageDef(frame(1));
    await tick();
    expect(s.fetched).toEqual([sha(1)]);
    s.handle.onFullJoin();
    s.handle.markLive();
    s.handle.onImageDef(frame(1));
    await tick();
    expect(s.fetched).toEqual([sha(1), sha(1)]);
  });

  it("setTheme recolours and dispose is idempotent", async () => {
    const s = setup();
    await loadedPicture(s);
    s.handle.setTheme("rgb(1, 2, 3)");
    const live = s.fake.decorations.filter((d) => !d.disposed);
    expect(live.length).toBeGreaterThan(0);
    expect(live[0].opts.foregroundColor).toBe("#010203");
    s.handle.dispose();
    s.handle.dispose();
    expect(s.fake.liveDecorations).toBe(0);
  });
});
