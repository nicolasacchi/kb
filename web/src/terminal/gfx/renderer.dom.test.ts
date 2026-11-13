// @vitest-environment jsdom
import { afterEach, describe, expect, it } from "vitest";
import { ImageDefStore } from "./imageDefs";
import { MediaCache } from "./media";
import { normalizeBg, PlaceholderRenderer, type Metrics } from "./renderer";
import { FakeTerm } from "./fakeTerm";
import type { ImageDefFrame } from "../wire";

const SHA = "d".repeat(64);
const ID = 0x010203;

function def(over: Partial<ImageDefFrame> = {}): ImageDefFrame {
  return {
    t: "imagedef", seq: 1, id: ID, gen: 1, sha256: SHA, mime: "image/png",
    w: 400, h: 200, cols: 4, rows: 3, offset: 0, ...over,
  };
}

function setup(metrics: Partial<Metrics> = {}, bg = "#000000") {
  const fake = new FakeTerm(20, 10);
  const defs = new ImageDefStore();
  const media = new MediaCache({
    fetchImpl: (() => Promise.resolve(new Response(new Uint8Array(4), { status: 200 }))) as typeof fetch,
    createObjectURL: () => "blob:test/pic",
    revokeObjectURL: () => {},
    decode: () => Promise.resolve(),
    sleep: () => Promise.resolve(),
  });
  let measured = 0;
  const m: Metrics = { cw: 10, ch: 20, dpr: 1, viewportY: 0, ...metrics };
  const renderer = new PlaceholderRenderer(fake.asTerm(), defs, media, {
    bg,
    measure: () => {
      measured += 1;
      m.viewportY = fake.viewportY;
      return m;
    },
    requestFrame: () => 0,
    cancelFrame: () => {},
  });
  return { fake, defs, media, renderer, measured: () => measured, m };
}

const clips = (fake: FakeTerm): HTMLElement[] =>
  [...fake.screen.querySelectorAll<HTMLElement>(".kbgfx-clip")];
const px = (v: string): number => Number.parseFloat(v);

afterEach(() => {
  document.body.innerHTML = "";
});

describe("PlaceholderRenderer", () => {
  it("clip rect is the snapped visible cell bounds", () => {
    const { fake, renderer } = setup({ cw: 8.25, ch: 17.5, dpr: 2 });
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    const [clip] = clips(fake);
    // columns 2..5 -> 16.5 .. 49.5 css px; rows 1..3 -> 17.5 .. 70, snapped to device px.
    expect(px(clip.style.left)).toBe(16.5);
    expect(px(clip.style.top)).toBe(17.5);
    expect(px(clip.style.width)).toBe(33);
    expect(px(clip.style.height)).toBe(52.5);
    expect(clip.dataset.kbgfxId).toBe(String(ID));
  });

  it("picture box uses ImageDef cols x rows", async () => {
    const { fake, renderer, defs, media } = setup();
    defs.apply(def({ cols: 8, rows: 6 }));
    await media.load(SHA);
    // Only 4x3 cells are on screen: the picture box is still 8x6 cells.
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    const img = fake.screen.querySelector<HTMLElement>(".kbgfx-img");
    expect(img).not.toBeNull();
    expect(px(img!.style.left)).toBe(0);
    expect(px(img!.style.top)).toBe(0);
    expect(px(img!.style.width)).toBe(80);
    expect(px(img!.style.height)).toBe(120);
    expect((img as HTMLImageElement).src).toContain("blob:test/pic");
    expect(renderer.stats.imgCreated).toBe(1);
  });

  it("falls back to the visible extent while the def is unknown", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 0, 0, 5, 2);
    renderer.render();
    expect(clips(fake)).toHaveLength(1);
    expect(fake.screen.querySelector(".kbgfx-missing")).not.toBeNull();
  });

  it("top crop when origin row is above the viewport", async () => {
    const { fake, renderer, defs, media } = setup();
    defs.apply(def({ cols: 4, rows: 6 }));
    await media.load(SHA);
    // Picture starts at abs line 0, viewport starts at line 2: rows 2..5 show.
    fake.picture(ID, 0, 1, 4, 6);
    fake.viewportY = 2;
    fake.baseY = 2;
    renderer.render();
    const [clip] = clips(fake);
    expect(px(clip.style.top)).toBe(0);
    expect(px(clip.style.height)).toBe(80);
    const img = clip.querySelector<HTMLElement>(".kbgfx-img")!;
    expect(px(img.style.top)).toBe(-40);
    expect(px(img.style.height)).toBe(120);
    expect(px(img.style.left)).toBe(0);
    expect(px(img.style.width)).toBe(40);
  });

  it("unknown id shows image unavailable then swaps to the picture when the def and bytes arrive", async () => {
    const { fake, renderer, defs, media } = setup();
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    const missing = fake.screen.querySelector<HTMLElement>(".kbgfx-missing");
    expect(missing?.textContent).toBe("image unavailable");
    const clip = clips(fake)[0];
    expect(px(missing!.style.width)).toBe(px(clip.style.width));
    expect(px(missing!.style.height)).toBe(px(clip.style.height));
    // The def arrives, bytes still loading: a neutral box without the error text.
    defs.apply(def());
    renderer.render();
    expect(fake.screen.querySelector(".kbgfx-missing")).toBeNull();
    expect(fake.screen.querySelector(".kbgfx-pending")?.textContent).toBe("");
    // Bytes arrive: the picture replaces the box.
    await media.load(SHA);
    renderer.render();
    expect(fake.screen.querySelector(".kbgfx-pending")).toBeNull();
    expect(fake.screen.querySelectorAll(".kbgfx-img")).toHaveLength(1);
    expect(clips(fake)).toHaveLength(1);
  });

  it("a sticky missing picture shows image unavailable", async () => {
    const { fake, renderer, defs } = setup();
    const media = new MediaCache({
      fetchImpl: (() => Promise.resolve(new Response(null, { status: 404 }))) as typeof fetch,
      sleep: () => Promise.resolve(),
    });
    const r2 = new PlaceholderRenderer(fake.asTerm(), defs, media, {
      bg: "#000000",
      measure: () => ({ cw: 10, ch: 20, dpr: 1, viewportY: 0 }),
      requestFrame: () => 0,
      cancelFrame: () => {},
    });
    renderer.dispose();
    defs.apply(def());
    await media.load(SHA);
    fake.picture(ID, 0, 0, 2, 2);
    r2.render();
    expect(fake.screen.querySelector(".kbgfx-missing")?.textContent).toBe("image unavailable");
    r2.dispose();
  });

  it("a new def version swaps the img src", async () => {
    const { fake, renderer, defs, media } = setup();
    defs.apply(def());
    await media.load(SHA);
    fake.picture(ID, 0, 0, 4, 3);
    renderer.render();
    const img = fake.screen.querySelector<HTMLImageElement>(".kbgfx-img")!;
    const first = img.src;
    const sha2 = "e".repeat(64);
    defs.apply(def({ sha256: sha2 }));
    // Same element would keep the old picture until bytes for the new sha load.
    renderer.render();
    expect(fake.screen.querySelector(".kbgfx-img")).toBeNull();
    expect(fake.screen.querySelector(".kbgfx-pending")).not.toBeNull();
    await media.load(sha2);
    renderer.render();
    expect(fake.screen.querySelectorAll(".kbgfx-img")).toHaveLength(1);
    expect(first).toContain("blob:");
    expect(defs.get(ID)?.version).toBe(2);
  });

  it("decorations cover exactly the placeholder runs", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 1, 2, 4, 2);
    fake.picture(0x0a0b0c, 1, 10, 3, 1);
    fake.text(2, 0, "hi");
    renderer.render();
    const made = fake.decorations.map((d) => ({
      line: (d.opts.marker as unknown as { line: number }).line,
      x: d.opts.x,
      width: d.opts.width,
      height: d.opts.height,
      fg: d.opts.foregroundColor,
      bg: d.opts.backgroundColor,
      layer: d.opts.layer,
    }));
    const key = (o: { line: number; x: number }): number => o.line * 100 + o.x;
    expect(made.sort((a, b) => key(a) - key(b))).toEqual([
      { line: 1, x: 2, width: 4, height: 1, fg: "#000000", bg: "#000000", layer: "top" },
      { line: 1, x: 10, width: 3, height: 1, fg: "#000000", bg: "#000000", layer: "top" },
      { line: 2, x: 2, width: 4, height: 1, fg: "#000000", bg: "#000000", layer: "top" },
    ]);
    expect(renderer.stats.decosCreated).toBe(3);
    // A second render with nothing changed creates nothing new.
    renderer.render();
    expect(renderer.stats.decosCreated).toBe(3);
    expect(renderer.liveDecorations).toBe(3);
  });

  it("decorations are disposed when the run disappears", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 1, 2, 4, 2);
    renderer.render();
    expect(renderer.liveDecorations).toBe(2);
    // Overwrite line 2 with text: its run is gone, line 1's stays.
    fake.lines[2] = [];
    fake.text(2, 0, "plain text");
    renderer.render();
    expect(renderer.liveDecorations).toBe(1);
    expect(renderer.stats.decosDisposed).toBe(1);
    expect(fake.decorations.filter((d) => d.disposed)).toHaveLength(1);
    expect(clips(fake)).toHaveLength(1);
  });

  it("ED3 repaint leaves no decoration leak", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    expect(renderer.liveDecorations).toBe(3);
    // Clear (markers die, xterm disposes their decorations), then the same
    // picture is reprinted.
    fake.clearAll();
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    const s = renderer.stats;
    expect(s.decosCreated - s.decosDisposed).toBe(renderer.liveDecorations);
    expect(renderer.liveDecorations).toBe(3);
    expect(fake.liveDecorations).toBe(3);
    // And a clear with nothing reprinted drains to zero.
    fake.clearAll();
    renderer.render();
    expect(renderer.stats.decosCreated - renderer.stats.decosDisposed).toBe(0);
    expect(renderer.liveDecorations).toBe(0);
    expect(clips(fake)).toHaveLength(0);
  });

  it("setTheme disposes and recreates decorations with the new background", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 1, 2, 4, 2);
    renderer.render();
    const before = fake.decorations.slice();
    expect(before).toHaveLength(2);
    renderer.setTheme("#112233");
    expect(before.every((d) => d.disposed)).toBe(true);
    const after = fake.decorations.filter((d) => !d.disposed);
    expect(after).toHaveLength(2);
    for (const d of after) {
      expect(d.opts.foregroundColor).toBe("#112233");
      expect(d.opts.backgroundColor).toBe("#112233");
    }
    expect(clips(fake)[0].style.background).toMatch(/#112233|rgb\(17, 34, 51\)/);
    const s = renderer.stats;
    expect(s.decosCreated - s.decosDisposed).toBe(renderer.liveDecorations);
  });

  it("dispose removes overlay and all subscriptions", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 1, 2, 4, 2);
    renderer.render();
    expect(fake.screen.querySelector(".kbgfx-overlay")).not.toBeNull();
    expect(fake.listenerCount).toBe(3);
    renderer.dispose();
    expect(fake.screen.querySelector(".kbgfx-overlay")).toBeNull();
    expect(fake.listenerCount).toBe(0);
    expect(fake.liveDecorations).toBe(0);
    // Idempotent, and a late event is a no-op.
    renderer.dispose();
    fake.fire("render");
    expect(fake.screen.querySelector(".kbgfx-overlay")).toBeNull();
  });

  it("no work when no placeholder cell", () => {
    const { fake, renderer, measured } = setup();
    fake.text(0, 0, "just text");
    renderer.render();
    fake.fire("render");
    expect(measured()).toBe(0);
    expect(fake.markers).toHaveLength(0);
    expect(fake.decorations).toHaveLength(0);
    expect(fake.screen.querySelector(".kbgfx-clip")).toBeNull();
    expect(renderer.stats.frames).toBe(0);
  });

  it("repaints on the terminal render event", () => {
    const { fake, renderer } = setup();
    fake.picture(ID, 0, 0, 2, 2);
    fake.fire("render");
    expect(clips(fake)).toHaveLength(1);
    expect(renderer.stats.frames).toBe(1);
  });

  it("hitTest finds only loaded pictures", async () => {
    const { fake, renderer, defs, media } = setup();
    defs.apply(def());
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    // Not loaded yet: a neutral box is not an openable picture.
    expect(renderer.hitTest(5, 5)).toBeNull();
    await media.load(SHA);
    renderer.render();
    const clip = clips(fake)[0];
    clip.getBoundingClientRect = () =>
      ({ left: 20, top: 20, right: 60, bottom: 80, width: 40, height: 60 }) as DOMRect;
    expect(renderer.hitTest(30, 30)).toBe(ID);
    expect(renderer.hitTest(200, 30)).toBeNull();
  });
});

describe("normalizeBg", () => {
  it("normalises css colours to #rrggbb", () => {
    expect(normalizeBg("#ABCDEF")).toBe("#abcdef");
    expect(normalizeBg("#abc")).toBe("#aabbcc");
    expect(normalizeBg("#11223344")).toBe("#112233");
    expect(normalizeBg("rgb(1, 2, 3)")).toBe("#010203");
    expect(normalizeBg("rgba(255, 128, 0, 0.5)")).toBe("#ff8000");
    expect(normalizeBg("")).toBe("#000000");
    expect(normalizeBg("not a colour")).toBe("#000000");
  });
});
