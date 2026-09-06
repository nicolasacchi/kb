import { describe, expect, it } from "vitest";
import { clampStep, initialStepIndex, playbackSteps } from "./storyPlayer";
import type { CommitSummary } from "../api/types";

function commit(sha: string, subject: string, author_time = 0): CommitSummary {
  return { sha, subject, author: "Test <t@example.com>", author_time };
}

describe("playbackSteps", () => {
  it("reverses newest-first entries into oldest-first playback order", () => {
    const entries = [commit("c3", "third"), commit("c2", "second"), commit("c1", "first")];
    expect(playbackSteps(entries).map((s) => s.sha)).toEqual(["c1", "c2", "c3"]);
  });

  it("does not mutate the input array", () => {
    const entries = [commit("c2", "second"), commit("c1", "first")];
    const copy = [...entries];
    playbackSteps(entries);
    expect(entries).toEqual(copy);
  });

  it("handles an empty list", () => {
    expect(playbackSteps([])).toEqual([]);
  });
});

describe("initialStepIndex", () => {
  const steps = [commit("aaa111", "first"), commit("bbb222", "second"), commit("ccc333", "third")];

  it("defaults to 0 (oldest) when no ?at= sha is given", () => {
    expect(initialStepIndex(steps, undefined)).toBe(0);
  });

  it("resolves an exact sha match", () => {
    expect(initialStepIndex(steps, "bbb222")).toBe(1);
  });

  it("resolves a short-prefix sha", () => {
    expect(initialStepIndex(steps, "ccc")).toBe(2);
  });

  it("resolves a longer-than-stored ref that starts with the stored sha", () => {
    expect(initialStepIndex(steps, "aaa111extra")).toBe(0);
  });

  it("defaults to 0 for a sha matching no step", () => {
    expect(initialStepIndex(steps, "deadbeef")).toBe(0);
  });

  it("defaults to 0 for an empty steps list regardless of sha", () => {
    expect(initialStepIndex([], "whatever")).toBe(0);
  });
});

describe("clampStep", () => {
  it("clamps a negative index up to 0", () => {
    expect(clampStep(-3, 5)).toBe(0);
  });

  it("clamps an over-range index down to length - 1", () => {
    expect(clampStep(99, 5)).toBe(4);
  });

  it("passes an in-range index through unchanged", () => {
    expect(clampStep(2, 5)).toBe(2);
  });

  it("returns 0 for a zero-length list", () => {
    expect(clampStep(3, 0)).toBe(0);
    expect(clampStep(-1, 0)).toBe(0);
  });
});
