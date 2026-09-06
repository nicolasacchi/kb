import { describe, expect, it } from "vitest";
import { fitTransform, logicalToScreen, screenToLogical } from "./atlasFit";

const W = 600;
const H = 360;

describe("fitTransform", () => {
  it("exact-aspect container (dpr 1) has scale 1 and zero offsets", () => {
    const fit = fitTransform(W, H, 1, W, H);
    expect(fit.scale).toBeCloseTo(1);
    expect(fit.offsetX).toBeCloseTo(0);
    expect(fit.offsetY).toBeCloseTo(0);
  });

  it("wider container letterboxes horizontally (pillarbox)", () => {
    // Same aspect (600/360) scaled up in width only: height is the
    // limiting axis, scale === cssH/H, and the extra width is centered.
    const fit = fitTransform(1200, 360, 1, W, H);
    expect(fit.scale).toBeCloseTo(1);
    expect(fit.offsetY).toBeCloseTo(0);
    expect(fit.offsetX).toBeCloseTo((1200 - W) / 2);
    expect(fit.offsetX).toBeGreaterThan(0);
  });

  it("taller container letterboxes vertically", () => {
    const fit = fitTransform(600, 720, 1, W, H);
    expect(fit.scale).toBeCloseTo(1);
    expect(fit.offsetX).toBeCloseTo(0);
    expect(fit.offsetY).toBeCloseTo((720 - H) / 2);
    expect(fit.offsetY).toBeGreaterThan(0);
  });

  it("scales uniformly on both axes even for a non-matching aspect", () => {
    // A container way wider than the logical aspect: scale must come from
    // the min ratio (height), and both the drawn width AND height use the
    // SAME scale — this is the defect fix: no ax != ay split.
    const fit = fitTransform(2000, 400, 1, W, H);
    const scaleX = 2000 / W;
    const scaleY = 400 / H;
    expect(fit.scale).toBeCloseTo(Math.min(scaleX, scaleY));
    expect(fit.scale).not.toBeCloseTo(Math.max(scaleX, scaleY));
  });

  it("honours dpr as a uniform multiplier on scale and offsets", () => {
    const fit1 = fitTransform(1200, 360, 1, W, H);
    const fit2 = fitTransform(1200, 360, 2, W, H);
    expect(fit2.scale).toBeCloseTo(fit1.scale * 2);
    expect(fit2.offsetX).toBeCloseTo(fit1.offsetX * 2);
    expect(fit2.offsetY).toBeCloseTo(fit1.offsetY * 2);
  });

  it("returns a degenerate all-zero fit for a zero-width container", () => {
    const fit = fitTransform(0, 360, 1, W, H);
    expect(fit.scale).toBe(0);
    expect(fit.offsetX).toBe(0);
    expect(fit.offsetY).toBe(0);
    expect(Number.isNaN(fit.scale)).toBe(false);
  });

  it("returns a degenerate all-zero fit for a zero-height container", () => {
    const fit = fitTransform(600, 0, 1, W, H);
    expect(fit.scale).toBe(0);
    expect(Number.isNaN(fit.offsetY)).toBe(false);
  });

  it("returns a degenerate all-zero fit for a zero logical size", () => {
    const fit = fitTransform(600, 360, 1, 0, H);
    expect(fit.scale).toBe(0);
    expect(fit.offsetX).toBe(0);
  });

  it("never produces NaN/Infinity for non-finite dpr", () => {
    const fit = fitTransform(600, 360, Number.NaN, W, H);
    expect(Number.isFinite(fit.scale)).toBe(true);
    expect(Number.isFinite(fit.offsetX)).toBe(true);
    expect(Number.isFinite(fit.offsetY)).toBe(true);
  });
});

describe("screenToLogical / logicalToScreen round-trip", () => {
  const cases: {
    name: string;
    cssW: number;
    cssH: number;
    dpr: number;
    pan: { x: number; y: number };
    zoom: number;
  }[] = [
    { name: "identity", cssW: W, cssH: H, dpr: 1, pan: { x: 0, y: 0 }, zoom: 1 },
    {
      name: "pillarboxed + panned + zoomed, dpr 1",
      cssW: 1200,
      cssH: 360,
      dpr: 1,
      pan: { x: 40, y: -12 },
      zoom: 2.5,
    },
    {
      name: "letterboxed vertically, dpr 2 (mobile 70vh case)",
      cssW: 600,
      cssH: 900,
      dpr: 2,
      pan: { x: -80, y: 15 },
      zoom: 0.75,
    },
  ];

  for (const c of cases) {
    it(`round-trips a set of logical points: ${c.name}`, () => {
      const fit = fitTransform(c.cssW, c.cssH, c.dpr, W, H);
      const points = [
        { x: 0, y: 0 },
        { x: W, y: H },
        { x: 300, y: 180 },
        { x: 24, y: 336 },
      ];
      for (const p of points) {
        const screen = logicalToScreen(p, fit, c.pan, c.zoom);
        const back = screenToLogical(screen, fit, c.pan, c.zoom);
        expect(back).not.toBeNull();
        expect(back!.x).toBeCloseTo(p.x, 6);
        expect(back!.y).toBeCloseTo(p.y, 6);
      }
    });
  }

  it("screenToLogical returns null (never NaN) for a degenerate fit", () => {
    const fit = fitTransform(0, 0, 1, W, H);
    const back = screenToLogical({ x: 10, y: 10 }, fit, { x: 0, y: 0 }, 1);
    expect(back).toBeNull();
  });

  it("screenToLogical returns null for a zero zoom instead of dividing by zero", () => {
    const fit = fitTransform(W, H, 1, W, H);
    const back = screenToLogical({ x: 10, y: 10 }, fit, { x: 0, y: 0 }, 0);
    expect(back).toBeNull();
  });

  it("a circle stays a circle: uniform scale means equal x/y magnification", () => {
    const fit = fitTransform(2000, 400, 1, W, H);
    // Two logical points offset by the same delta on each axis must map to
    // the SAME on-screen delta on each axis when the container is wider
    // than the logical aspect (the ax != ay defect this unit fixes).
    const a = logicalToScreen({ x: 100, y: 100 }, fit, { x: 0, y: 0 }, 1);
    const b = logicalToScreen({ x: 110, y: 110 }, fit, { x: 0, y: 0 }, 1);
    expect(b.x - a.x).toBeCloseTo(b.y - a.y, 6);
  });
});
