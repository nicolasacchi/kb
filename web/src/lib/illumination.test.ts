import { describe, expect, it } from "vitest";
import { generateIllumination, illuminationSvg, VIEW_BOX } from "./illumination";

describe("generateIllumination", () => {
  it("is deterministic: same seed → identical spec", () => {
    const a = generateIllumination("a1b2c3d4e5f6");
    const b = generateIllumination("a1b2c3d4e5f6");
    expect(a).toEqual(b);
  });

  it("produces 2-4 motifs from the closed vocabulary", () => {
    for (const seed of ["000000000000", "a1b2c3d4e5f6", "ffffffffffff", "id-with-dashes"]) {
      const spec = generateIllumination(seed);
      expect(spec.motifs.length).toBeGreaterThanOrEqual(2);
      expect(spec.motifs.length).toBeLessThanOrEqual(4);
      for (const m of spec.motifs) {
        expect(["arc", "dots", "corner"]).toContain(m.kind);
        expect(["dim", "accent", "current"]).toContain(m.color);
      }
    }
  });

  it("keeps every motif's geometry inside the 24x24 viewBox", () => {
    for (const seed of ["000000000000", "a1b2c3d4e5f6", "ffffffffffff", "another-seed"]) {
      const spec = generateIllumination(seed);
      for (const m of spec.motifs) {
        if (m.kind === "arc") {
          expect(m.cx).toBeGreaterThanOrEqual(0);
          expect(m.cx).toBeLessThanOrEqual(VIEW_BOX);
          expect(m.cy).toBeGreaterThanOrEqual(0);
          expect(m.cy).toBeLessThanOrEqual(VIEW_BOX);
        } else if (m.kind === "dots") {
          const lastDotX = m.cx + (m.count - 1) * m.spacing;
          expect(lastDotX).toBeLessThanOrEqual(VIEW_BOX + m.spacing); // rotation may carry it past cx alone
        } else {
          expect(m.x).toBeGreaterThanOrEqual(0);
          expect(m.x).toBeLessThanOrEqual(VIEW_BOX);
          expect(m.y).toBeGreaterThanOrEqual(0);
          expect(m.y).toBeLessThanOrEqual(VIEW_BOX);
        }
      }
    }
  });

  it("spreads seeds across more than one motif count / kind (not a constant)", () => {
    const specs = Array.from({ length: 20 }, (_, i) => generateIllumination(`seed-${i}`));
    const counts = new Set(specs.map((s) => s.motifs.length));
    const kinds = new Set(specs.flatMap((s) => s.motifs.map((m) => m.kind)));
    expect(counts.size).toBeGreaterThan(1);
    expect(kinds.size).toBeGreaterThan(1);
  });

  it("dot rows only rotate by a multiple of 45deg; corners only by a multiple of 90deg", () => {
    for (const seed of ["000000000000", "a1b2c3d4e5f6", "ffffffffffff", "yet-another"]) {
      for (const m of generateIllumination(seed).motifs) {
        if (m.kind === "dots") expect(m.rotateDeg % 45).toBe(0);
        if (m.kind === "corner") expect(m.rotateDeg % 90).toBe(0);
      }
    }
  });
});

describe("illuminationSvg", () => {
  it("is deterministic and produces well-formed, theme-safe markup", () => {
    const svg = illuminationSvg("a1b2c3d4e5f6");
    expect(svg).toBe(illuminationSvg("a1b2c3d4e5f6"));
    expect(svg.startsWith(`<svg viewBox="0 0 ${VIEW_BOX} ${VIEW_BOX}"`)).toBe(true);
    expect(svg).toContain('aria-hidden="true"');
    // No baked hex/rgb colors — only currentColor or the two CSS-var slots.
    expect(svg).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
    expect(svg).not.toMatch(/rgb\(/);
  });

  it("differs across different seeds", () => {
    const svgs = new Set(
      ["a", "b", "c", "d", "e"].map((s) => illuminationSvg(s.repeat(12))),
    );
    expect(svgs.size).toBeGreaterThan(1);
  });

  // Golden pin — exact byte-for-byte output for a fixed seed (chosen because
  // it happens to exercise all 3 motif kinds in one ornament). If this ever
  // legitimately changes (a generator tweak), regenerate via
  // `illuminationSvg("000000000000")` and paste the new value here.
  it("golden: exact SVG string for a fixed seed", () => {
    expect(illuminationSvg("000000000000")).toBe(
      '<svg viewBox="0 0 24 24" width="24" height="24" aria-hidden="true">' +
        '<path d="M 9.86 21.22 L 9.86 15.1 L 15.98 15.1" fill="none" ' +
        'stroke="var(--card-accent, var(--accent))" stroke-width="1.2" ' +
        'stroke-linecap="round" stroke-linejoin="round" ' +
        'transform="rotate(90 9.86 15.1)" />' +
        '<g transform="rotate(45 10.94 7.3)">' +
        '<circle cx="10.94" cy="7.3" r="1.14" fill="currentColor" />' +
        '<circle cx="14.42" cy="7.3" r="1.14" fill="currentColor" />' +
        '<circle cx="17.9" cy="7.3" r="1.14" fill="currentColor" />' +
        '<circle cx="21.38" cy="7.3" r="1.14" fill="currentColor" />' +
        "</g>" +
        '<circle cx="5.45" cy="7.36" r="5.19" fill="none" ' +
        'stroke="var(--card-accent, var(--accent))" stroke-width="1.2" ' +
        'stroke-linecap="round" pathLength="100" stroke-dasharray="20.21 100" ' +
        'stroke-dashoffset="-52.02" />' +
        "</svg>",
    );
  });

  it("golden: exact motif spec for the same fixed seed", () => {
    expect(generateIllumination("000000000000")).toEqual({
      seed: "000000000000",
      motifs: [
        {
          kind: "corner",
          x: 9.86,
          y: 15.1,
          size: 6.12,
          rotateDeg: 90,
          color: "accent",
        },
        {
          kind: "dots",
          cx: 10.94,
          cy: 7.3,
          count: 4,
          spacing: 3.48,
          r: 1.14,
          rotateDeg: 45,
          color: "current",
        },
        {
          kind: "arc",
          cx: 5.45,
          cy: 7.36,
          r: 5.19,
          sweepPct: 20.21,
          offsetPct: 52.02,
          color: "accent",
        },
      ],
    });
  });
});
