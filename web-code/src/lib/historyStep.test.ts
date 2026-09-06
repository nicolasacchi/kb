import { describe, expect, it } from "vitest";
import { currentHistoryIndex, historyStepTarget } from "./historyStep";
import type { CommitSummary } from "../api/types";

function summary(sha: string, author_time: number): CommitSummary {
  return { sha, subject: `commit ${sha}`, author: "Test <t@example.com>", author_time };
}

const ENTRIES: CommitSummary[] = [summary("cccccccccccccccccccccccccccccccccccccccc", 300), summary("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", 200), summary("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 100)];

describe("currentHistoryIndex", () => {
  it("is -1 for the working tree (undefined ref)", () => {
    expect(currentHistoryIndex(ENTRIES, undefined)).toBe(-1);
  });

  it("finds an exact sha match", () => {
    expect(currentHistoryIndex(ENTRIES, ENTRIES[1].sha)).toBe(1);
  });

  it("matches a short ref prefix against a full entry sha", () => {
    expect(currentHistoryIndex(ENTRIES, "bbbbbbb")).toBe(1);
  });

  it("matches a full ref against a (hypothetically) short entry sha", () => {
    const shortEntries: CommitSummary[] = [summary("abc1234", 100)];
    expect(currentHistoryIndex(shortEntries, "abc1234567890")).toBe(0);
  });

  it("is -1 for an unrecognized ref", () => {
    expect(currentHistoryIndex(ENTRIES, "main")).toBe(-1);
  });
});

describe("historyStepTarget", () => {
  it("stepping older from the working tree lands on the newest entry", () => {
    expect(historyStepTarget(ENTRIES, -1, -1)).toEqual({ kind: "navigate", sha: ENTRIES[0].sha });
  });

  it("stepping older walks the list one entry at a time", () => {
    expect(historyStepTarget(ENTRIES, 0, -1)).toEqual({ kind: "navigate", sha: ENTRIES[1].sha });
    expect(historyStepTarget(ENTRIES, 1, -1)).toEqual({ kind: "navigate", sha: ENTRIES[2].sha });
  });

  it("stepping older past the last entry warns 'oldest commit'", () => {
    expect(historyStepTarget(ENTRIES, 2, -1)).toEqual({ kind: "warn", message: "oldest commit" });
  });

  it("stepping newer from the newest entry (index 0) returns to the working tree", () => {
    expect(historyStepTarget(ENTRIES, 0, 1)).toEqual({ kind: "navigate", sha: undefined });
  });

  it("stepping newer walks the list back toward the working tree", () => {
    expect(historyStepTarget(ENTRIES, 2, 1)).toEqual({ kind: "navigate", sha: ENTRIES[1].sha });
    expect(historyStepTarget(ENTRIES, 1, 1)).toEqual({ kind: "navigate", sha: ENTRIES[0].sha });
  });

  it("stepping newer from the working tree warns 'back to working tree'", () => {
    expect(historyStepTarget(ENTRIES, -1, 1)).toEqual({ kind: "warn", message: "back to working tree" });
  });

  it("warns when the file has no history at all, either direction", () => {
    expect(historyStepTarget([], -1, -1)).toEqual({ kind: "warn", message: "no history for this file" });
    expect(historyStepTarget([], -1, 1)).toEqual({ kind: "warn", message: "no history for this file" });
  });
});
