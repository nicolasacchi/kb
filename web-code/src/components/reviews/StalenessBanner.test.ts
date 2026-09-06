import { describe, expect, it } from "vitest";
import type { ReviewDetail } from "../../api/types";
import { prHeadDriftMessage, stalenessMessage } from "./StalenessBanner";

function review(overrides: Partial<Pick<ReviewDetail, "verdict" | "verdict_stale">> = {}) {
  return {
    verdict: null,
    verdict_stale: false,
    ...overrides,
  } as Pick<ReviewDetail, "verdict" | "verdict_stale">;
}

describe("stalenessMessage", () => {
  it("is null when verdict_stale is false", () => {
    const r = review({ verdict_stale: false, verdict: { state: "approve", note: null, at: 1, ps: 1 } });
    expect(stalenessMessage(r, 2)).toBeNull();
  });

  it("is null when verdict_stale is true but no verdict is set (shouldn't happen, but honest)", () => {
    expect(stalenessMessage(review({ verdict_stale: true, verdict: null }), 2)).toBeNull();
  });

  it("names both patchset numbers when both are known", () => {
    const r = review({ verdict_stale: true, verdict: { state: "comment", note: null, at: 1, ps: 1 } });
    expect(stalenessMessage(r, 3)).toBe("Your verdict was set at ps1 — ps3 has landed since.");
  });

  it("degrades to a generic message when latestPs is unknown", () => {
    const r = review({ verdict_stale: true, verdict: { state: "comment", note: null, at: 1, ps: 1 } });
    expect(stalenessMessage(r, null)).toBe(
      "Your verdict is stale — a newer patchset has landed since it was set.",
    );
  });
});

describe("prHeadDriftMessage", () => {
  it("is null when no drift signal is present (R4 not landed yet, or no drift)", () => {
    expect(prHeadDriftMessage(null)).toBeNull();
    expect(prHeadDriftMessage(undefined)).toBeNull();
  });

  it("shortens both shas to 12 chars", () => {
    const msg = prHeadDriftMessage({
      fromSha: "41ac09eaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      toSha: "8f21bb0bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    });
    expect(msg).toContain("41ac09eaaaaa");
    expect(msg).not.toContain("41ac09eaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
  });

  it("names zero newly-orphaned findings honestly, not silently", () => {
    const msg = prHeadDriftMessage({ fromSha: "a".repeat(40), toSha: "b".repeat(40), newlyOrphaned: 0 });
    expect(msg).toContain("0 orphaned so far");
  });

  it("pluralizes the orphaned-findings count correctly", () => {
    expect(
      prHeadDriftMessage({ fromSha: "a".repeat(40), toSha: "b".repeat(40), newlyOrphaned: 1 }),
    ).toContain("1 finding lost their anchor");
    expect(
      prHeadDriftMessage({ fromSha: "a".repeat(40), toSha: "b".repeat(40), newlyOrphaned: 2 }),
    ).toContain("2 findings lost their anchor");
  });
});
