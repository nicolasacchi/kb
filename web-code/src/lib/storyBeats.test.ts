import { describe, expect, it } from "vitest";
import type { StoryEntry } from "../api/types";
import { gapBeatLabel, gapDate, isGapBeat } from "./storyBeats";

function gap(overrides: Partial<StoryEntry> = {}): StoryEntry {
  return {
    confidence: "none",
    via: "no-match",
    first_seen: 1_700_000_000, // 2023-11-14 UTC
    last_seen: 1_702_000_000, // 2023-12-08 UTC
    lines_touched: 3,
    status: "gap",
    commit_count: 3,
    reason: "no-captured-session",
    ...overrides,
  };
}

describe("isGapBeat", () => {
  it("is true only for status gap", () => {
    expect(isGapBeat(gap())).toBe(true);
    expect(isGapBeat(gap({ status: "owns-lines" }))).toBe(false);
    expect(isGapBeat(gap({ status: "historical" }))).toBe(false);
  });
});

describe("gapDate", () => {
  it("renders a locale-independent UTC ISO date", () => {
    expect(gapDate(1_700_000_000)).toBe("2023-11-14");
  });
});

describe("gapBeatLabel", () => {
  it("renders count and date range for a multi-commit gap", () => {
    expect(gapBeatLabel(gap())).toBe(
      "no captured session for 3 commits (2023-11-14..2023-12-08)",
    );
  });

  it("collapses a same-day range and the plural for a single commit", () => {
    expect(
      gapBeatLabel(gap({ commit_count: 1, last_seen: 1_700_000_000 })),
    ).toBe("no captured session for 1 commit (2023-11-14)");
  });

  it("falls back to first_seen when last_seen is absent", () => {
    expect(gapBeatLabel(gap({ commit_count: 2, last_seen: undefined }))).toBe(
      "no captured session for 2 commits (2023-11-14)",
    );
  });

  it("names the weaker join-unavailable claim honestly", () => {
    expect(gapBeatLabel(gap({ reason: "join-unavailable" }))).toBe(
      "session join unavailable for 3 commits (2023-11-14..2023-12-08)",
    );
  });

  it("fail-honests to join-unavailable copy when reason is absent", () => {
    expect(gapBeatLabel(gap({ reason: undefined, commit_count: 2 }))).toBe(
      "session join unavailable for 2 commits (2023-11-14..2023-12-08)",
    );
  });
});
