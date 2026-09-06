import { describe, expect, it } from "vitest";
import { riskDialGeometry } from "./RiskDial";

const CIRCUMFERENCE = 2 * Math.PI * 26;

describe("riskDialGeometry", () => {
  it("draws a full arc at max score", () => {
    const g = riskDialGeometry(10, 10);
    expect(g.clamped).toBe(10);
    const [filled] = g.dashArray.split(" ").map(Number);
    expect(filled).toBeCloseTo(CIRCUMFERENCE, 1);
  });

  it("draws no arc at score 0", () => {
    const g = riskDialGeometry(0, 10);
    expect(g.clamped).toBe(0);
    const [filled] = g.dashArray.split(" ").map(Number);
    expect(filled).toBeCloseTo(0, 1);
  });

  it("draws half the circumference at half score", () => {
    const g = riskDialGeometry(5, 10);
    expect(g.clamped).toBe(5);
    const [filled] = g.dashArray.split(" ").map(Number);
    expect(filled).toBeCloseTo(CIRCUMFERENCE / 2, 0);
  });

  it("clamps a score above max down to max", () => {
    expect(riskDialGeometry(99, 10).clamped).toBe(10);
  });

  it("clamps a negative score up to 0", () => {
    expect(riskDialGeometry(-3, 10).clamped).toBe(0);
  });

  it("treats a non-finite score as 0 rather than crashing", () => {
    expect(riskDialGeometry(NaN, 10).clamped).toBe(0);
    expect(riskDialGeometry(Infinity, 10).clamped).toBe(0);
    expect(riskDialGeometry(-Infinity, 10).clamped).toBe(0);
  });

  it("rounds a fractional score to the nearest integer", () => {
    expect(riskDialGeometry(3.4, 10).clamped).toBe(3);
    expect(riskDialGeometry(3.6, 10).clamped).toBe(4);
  });

  it("falls back to max=10 for a non-positive max", () => {
    expect(riskDialGeometry(5, 0).clamped).toBe(5);
    expect(riskDialGeometry(5, -1).clamped).toBe(5);
  });

  it("dashArray's two numbers always sum to the circumference", () => {
    for (const score of [0, 1, 4, 7, 10]) {
      const g = riskDialGeometry(score, 10);
      const [filled, remainder] = g.dashArray.split(" ").map(Number);
      expect(filled + remainder).toBeCloseTo(CIRCUMFERENCE, 1);
    }
  });
});
