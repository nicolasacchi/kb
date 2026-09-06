import { describe, expect, it } from "vitest";
import { classify, expectedMs, liveSummaryFromBeacon, MIN_EXPECTED_MS } from "./reading";
import type { ReadingSectionBeacon } from "../api/generated/ReadingSectionBeacon";

// Golden-mirrors crates/kb-core/src/reading.rs #[test] expected_ms_floors_and_scales
// + classify_unseen_skim_read. If the Rust constants/logic change, BOTH must move
// in lock-step (dual-grammar discipline #25/#29).
describe("reading classifier (ported from reading.rs)", () => {
  it("expectedMs floors and scales", () => {
    expect(expectedMs(0)).toBe(MIN_EXPECTED_MS);
    expect(expectedMs(-5)).toBe(MIN_EXPECTED_MS);
    expect(expectedMs(200)).toBe(60_000);
    expect(expectedMs(100)).toBe(30_000);
    expect(expectedMs(1)).toBeGreaterThanOrEqual(MIN_EXPECTED_MS);
  });

  it("classify: unseen / skim / read", () => {
    expect(classify(0, 200, 0)).toBe("unseen");
    expect(classify(50_000, 200, 0)).toBe("unseen");
    expect(classify(0, 200, 1)).toBe("skim");
    expect(classify(29_999, 200, 1)).toBe("skim");
    expect(classify(30_000, 200, 1)).toBe("read");
    expect(classify(60_000, 200, 1)).toBe("read");
    expect(classify(249, 0, 1)).toBe("skim");
    expect(classify(250, 0, 1)).toBe("read");
  });
});

describe("liveSummaryFromBeacon", () => {
  const b = (
    id: string,
    idx: number,
    words: number,
    dwell_ms: number,
    enters: number,
  ): ReadingSectionBeacon => ({
    id,
    idx,
    text: id,
    level: 2,
    words,
    content_px: 100,
    dwell_ms,
    enters,
  });

  it("maps beacons → sections and words-weights read_pct", () => {
    // s1: 200 words, read (60s ≥ 30s). s2: 200 words, skim (0 dwell). →
    // read fraction = 200/400 = 50%.
    const s = liveSummaryFromBeacon(
      [b("s1", 0, 200, 60_000, 1), b("s2", 1, 200, 0, 1)],
      60_000,
      42,
    );
    expect(s.sections?.map((x) => x.state)).toEqual(["read", "skim"]);
    expect(s.read_pct).toBe(50);
    expect(s.completion_pct).toBe(42);
    expect(s.visit_count).toBe(1);
    // section_id is remapped from the beacon's `id`.
    expect(s.sections?.[0].section_id).toBe("s1");
  });

  it("empty beacon → zeroed single-visit summary", () => {
    const s = liveSummaryFromBeacon([], 0, 0);
    expect(s.read_pct).toBe(0);
    expect(s.sections).toEqual([]);
    expect(s.visit_count).toBe(1);
  });
});
