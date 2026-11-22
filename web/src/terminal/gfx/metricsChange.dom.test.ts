// @vitest-environment jsdom
// FUGFXLATE: a font change after the pictures were placed (a late fitter pass)
// moves every cell, and xterm does not always announce it with a render,
// scroll or resize event the renderer hears. The pictures must follow the
// cells anyway: on a metric-change notification, on the screen element's own
// size change, and on a verification frame after every layout.
import { afterEach, describe, expect, it } from "vitest";
import { ImageDefStore } from "./imageDefs";
import { MediaCache } from "./media";
import { PlaceholderRenderer, type Metrics } from "./renderer";
import { FakeTerm } from "./fakeTerm";

const ID = 0x010203;

function setup() {
  const fake = new FakeTerm(20, 10);
  const defs = new ImageDefStore();
  const media = new MediaCache({
    fetchImpl: (() => Promise.resolve(new Response(new Uint8Array(4), { status: 200 }))) as typeof fetch,
    createObjectURL: () => "blob:test/pic",
    revokeObjectURL: () => {},
    decode: () => Promise.resolve(),
    sleep: () => Promise.resolve(),
  });
  const m: Metrics = { cw: 10, ch: 20, dpr: 2, viewportY: 0 };
  const frames: Array<() => void> = [];
  const renderer = new PlaceholderRenderer(fake.asTerm(), defs, media, {
    bg: "#000000",
    measure: () => ({ ...m }),
    requestFrame: (cb) => frames.push(cb),
    cancelFrame: () => {},
  });
  const flush = (): void => {
    for (let i = 0; i < 5 && frames.length > 0; i += 1) for (const cb of frames.splice(0)) cb();
  };
  return { fake, renderer, m, flush, frames };
}

const clip = (fake: FakeTerm): HTMLElement => fake.screen.querySelector<HTMLElement>(".kbgfx-clip")!;

afterEach(() => {
  document.body.innerHTML = "";
  delete (globalThis as { ResizeObserver?: unknown }).ResizeObserver;
});

describe("pictures follow a cell-metric change", () => {
  it("a verification frame re-lays out when the metrics moved after the last layout", () => {
    const { fake, renderer, m, flush } = setup();
    fake.picture(ID, 1, 2, 4, 3);
    renderer.render();
    expect(clip(fake).style.left).toBe("20px");
    // The late font change: no render, scroll or resize event reaches the renderer.
    m.cw = 14;
    m.ch = 28;
    flush();
    expect(clip(fake).style.left).toBe("28px");
    expect(clip(fake).style.top).toBe("28px");
    expect(clip(fake).style.width).toBe("56px");
  });

  it("notifyMetricsChanged schedules a re-layout", () => {
    const { fake, renderer, m, flush } = setup();
    fake.picture(ID, 0, 3, 2, 2);
    renderer.render();
    flush();
    m.cw = 12;
    renderer.notifyMetricsChanged();
    flush();
    expect(clip(fake).style.left).toBe("36px");
  });

  it("the screen element's own resize re-lays out", () => {
    let fire: () => void = () => {};
    (globalThis as { ResizeObserver?: unknown }).ResizeObserver = class {
      constructor(cb: () => void) {
        fire = cb;
      }
      observe(): void {}
      disconnect(): void {}
    };
    const { fake, renderer, m, flush } = setup();
    fake.picture(ID, 0, 3, 2, 2);
    renderer.render();
    flush();
    m.cw = 12;
    fire();
    flush();
    expect(clip(fake).style.left).toBe("36px");
    renderer.dispose();
  });

  it("settles: a stable metric stops scheduling frames", () => {
    const { fake, renderer, frames, flush } = setup();
    fake.picture(ID, 0, 0, 2, 2);
    renderer.render();
    flush();
    expect(frames).toHaveLength(0);
  });
});
