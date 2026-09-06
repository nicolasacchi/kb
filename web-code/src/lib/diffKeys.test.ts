import { describe, expect, it } from "vitest";
import {
  buildThreadStops,
  initialDiffKeysState,
  nextUnviewedFileIdx,
  reduceDiffKeys,
  stepThreadStop,
  type DiffKeysState,
} from "./diffKeys";

const FILES = ["a.ts", "b.ts", "c.ts"];

function state(partial?: Partial<DiffKeysState>): DiffKeysState {
  return {
    files: FILES,
    cursor: { fileIdx: 0, hunkIdx: 0 },
    collapsed: new Set(),
    ...partial,
  };
}

describe("reduceDiffKeys", () => {
  it("wrap-at-ends CLAMPS — nextFile on last file is a no-op", () => {
    const s = state({ cursor: { fileIdx: 2, hunkIdx: 0 } });
    expect(reduceDiffKeys(s, { type: "nextFile" }, [1, 1, 1])).toBe(s);
  });

  it("wrap-at-ends CLAMPS — prevFile on first file is a no-op", () => {
    const s = state();
    expect(reduceDiffKeys(s, { type: "prevFile" }, [1, 1, 1])).toBe(s);
  });

  it("wrap-at-ends CLAMPS — nextHunk on last hunk of last file is a no-op", () => {
    const s = state({ cursor: { fileIdx: 2, hunkIdx: 1 } });
    expect(reduceDiffKeys(s, { type: "nextHunk" }, [2, 2, 2])).toBe(s);
  });

  it("wrap-at-ends CLAMPS — prevHunk on first hunk of first file is a no-op", () => {
    const s = state();
    expect(reduceDiffKeys(s, { type: "prevHunk" }, [2, 2, 2])).toBe(s);
  });

  it("hunk stepping advances within a file", () => {
    const next = reduceDiffKeys(state(), { type: "nextHunk" }, [3, 1, 1]);
    expect(next.cursor).toEqual({ fileIdx: 0, hunkIdx: 1 });
  });

  it("hunk stepping crosses into the next file when the current file's hunks are exhausted", () => {
    const s = state({ cursor: { fileIdx: 0, hunkIdx: 1 } });
    const next = reduceDiffKeys(s, { type: "nextHunk" }, [2, 3, 1]);
    expect(next.cursor).toEqual({ fileIdx: 1, hunkIdx: 0 });
  });

  it("hunk stepping on an unfetched (0-hunk) file is file-level", () => {
    const next = reduceDiffKeys(state(), { type: "nextHunk" }, [0, 2, 1]);
    expect(next.cursor).toEqual({ fileIdx: 1, hunkIdx: 0 });
  });

  it("prevHunk crosses into the previous file's last hunk", () => {
    const s = state({ cursor: { fileIdx: 1, hunkIdx: 0 } });
    const next = reduceDiffKeys(s, { type: "prevHunk" }, [3, 2, 1]);
    expect(next.cursor).toEqual({ fileIdx: 0, hunkIdx: 2 });
  });

  it("prevHunk onto an unfetched file lands at hunk 0", () => {
    const s = state({ cursor: { fileIdx: 1, hunkIdx: 0 } });
    const next = reduceDiffKeys(s, { type: "prevHunk" }, [0, 2, 1]);
    expect(next.cursor).toEqual({ fileIdx: 0, hunkIdx: 0 });
  });

  it("nextFile / prevFile reset hunkIdx to 0", () => {
    const s = state({ cursor: { fileIdx: 1, hunkIdx: 4 } });
    expect(reduceDiffKeys(s, { type: "nextFile" }, [1, 5, 1]).cursor).toEqual({
      fileIdx: 2,
      hunkIdx: 0,
    });
    expect(reduceDiffKeys(s, { type: "prevFile" }, [1, 5, 1]).cursor).toEqual({
      fileIdx: 0,
      hunkIdx: 0,
    });
  });

  it("firstFile / lastFile", () => {
    const s = state({ cursor: { fileIdx: 1, hunkIdx: 2 } });
    expect(reduceDiffKeys(s, { type: "firstFile" }).cursor).toEqual({ fileIdx: 0, hunkIdx: 0 });
    expect(reduceDiffKeys(s, { type: "lastFile" }).cursor).toEqual({ fileIdx: 2, hunkIdx: 0 });
  });

  it("gotoFile clamps and resets hunkIdx", () => {
    expect(reduceDiffKeys(state(), { type: "gotoFile", fileIdx: 2 }).cursor).toEqual({
      fileIdx: 2,
      hunkIdx: 0,
    });
    expect(reduceDiffKeys(state(), { type: "gotoFile", fileIdx: 99 }).cursor).toEqual({
      fileIdx: 2,
      hunkIdx: 0,
    });
    expect(reduceDiffKeys(state(), { type: "gotoFile", fileIdx: -3 }).cursor).toEqual({
      fileIdx: 0,
      hunkIdx: 0,
    });
  });

  it("toggleCollapse adds then removes the cursor file", () => {
    const added = reduceDiffKeys(state(), { type: "toggleCollapse" });
    expect(added.collapsed.has("a.ts")).toBe(true);
    const removed = reduceDiffKeys(added, { type: "toggleCollapse" });
    expect(removed.collapsed.has("a.ts")).toBe(false);
  });

  // Collapse only affects rendering. The cursor still lands on a collapsed
  // file so file/hunk stepping and `v`/`x` address it.
  it("collapse does NOT skip files — nextFile still lands on a collapsed neighbour", () => {
    const collapsed = new Set(["b.ts"]);
    const s = state({ collapsed });
    const next = reduceDiffKeys(s, { type: "nextFile" }, [1, 1, 1]);
    expect(next.cursor.fileIdx).toBe(1);
    expect(next.files[next.cursor.fileIdx]).toBe("b.ts");
    expect(next.collapsed.has("b.ts")).toBe(true);
  });

  it("hunk stepping also lands on a collapsed file (does not skip)", () => {
    const s = state({ collapsed: new Set(["b.ts"]), cursor: { fileIdx: 0, hunkIdx: 0 } });
    const next = reduceDiffKeys(s, { type: "nextHunk" }, [1, 2, 1]);
    expect(next.cursor).toEqual({ fileIdx: 1, hunkIdx: 0 });
    expect(next.collapsed.has("b.ts")).toBe(true);
  });

  it("empty files list is a no-op for every motion", () => {
    const s = initialDiffKeysState([]);
    for (const action of [
      { type: "nextHunk" as const },
      { type: "prevHunk" as const },
      { type: "nextFile" as const },
      { type: "prevFile" as const },
      { type: "firstFile" as const },
      { type: "lastFile" as const },
      { type: "toggleCollapse" as const },
      { type: "gotoFile" as const, fileIdx: 1 },
    ]) {
      expect(reduceDiffKeys(s, action, [])).toEqual(s);
    }
  });

  it("setFiles keeps the cursor on the same path when it still exists", () => {
    const s = state({ cursor: { fileIdx: 1, hunkIdx: 3 } });
    const next = reduceDiffKeys(s, { type: "setFiles", files: ["b.ts", "c.ts", "d.ts"] });
    expect(next.files).toEqual(["b.ts", "c.ts", "d.ts"]);
    expect(next.cursor).toEqual({ fileIdx: 0, hunkIdx: 0 });
  });
});

describe("nextUnviewedFileIdx", () => {
  it("finds the next unviewed file after fromIdx and does not wrap", () => {
    expect(nextUnviewedFileIdx(FILES, new Set(["a.ts"]), 0)).toBe(1);
    expect(nextUnviewedFileIdx(FILES, new Set(["a.ts", "b.ts"]), 0)).toBe(2);
    expect(nextUnviewedFileIdx(FILES, new Set(["a.ts", "b.ts", "c.ts"]), 0)).toBeNull();
    expect(nextUnviewedFileIdx(FILES, new Set(["a.ts"]), 2)).toBeNull();
  });
});

// --- PRR-U3 — t/T thread-or-finding stepping -------------------------------

describe("buildThreadStops", () => {
  const paths = ["a.ts", "b.ts"];
  const threadsByPath = new Map([
    ["a.ts", [{ id: "a-comment" }, { id: "a-concern" }, { id: "a-blocker" }]],
    ["b.ts", [{ id: "b-comment" }]],
  ]);
  const severityByThread = new Map([
    ["a-concern", "concern"],
    ["a-blocker", "blocker"],
    // a-comment / b-comment are plain comments — absent from this map.
  ]);

  it("orders findings first (most severe first) then comments, per file, in path order", () => {
    const stops = buildThreadStops(paths, threadsByPath, severityByThread, "all");
    expect(stops.map((s) => s.id)).toEqual(["a-blocker", "a-concern", "a-comment", "b-comment"]);
    expect(stops.map((s) => s.fileIdx)).toEqual([0, 0, 0, 1]);
  });

  it("overlay 'findings' drops plain comments", () => {
    const stops = buildThreadStops(paths, threadsByPath, severityByThread, "findings");
    expect(stops.map((s) => s.id)).toEqual(["a-blocker", "a-concern"]);
  });

  it("overlay 'comments' drops findings", () => {
    const stops = buildThreadStops(paths, threadsByPath, severityByThread, "comments");
    expect(stops.map((s) => s.id)).toEqual(["a-comment", "b-comment"]);
  });

  it("overlay 'none' yields no stops", () => {
    expect(buildThreadStops(paths, threadsByPath, severityByThread, "none")).toEqual([]);
  });

  it("a file with no threads contributes no stops", () => {
    const stops = buildThreadStops(["a.ts", "empty.ts"], threadsByPath, severityByThread, "all");
    expect(stops.filter((s) => s.path === "empty.ts")).toEqual([]);
  });
});

describe("stepThreadStop", () => {
  const stops = [
    { id: "s1", fileIdx: 0, path: "a.ts" },
    { id: "s2", fileIdx: 0, path: "a.ts" },
    { id: "s3", fileIdx: 1, path: "b.ts" },
  ];

  it("no current id: t lands on the first stop, T on the last", () => {
    expect(stepThreadStop(stops, null, 1)).toEqual(stops[0]);
    expect(stepThreadStop(stops, null, -1)).toEqual(stops[2]);
  });

  it("steps forward / backward from the current stop", () => {
    expect(stepThreadStop(stops, "s1", 1)).toEqual(stops[1]);
    expect(stepThreadStop(stops, "s2", -1)).toEqual(stops[0]);
  });

  it("wrap-at-ends CLAMPS — stepping past either end is a no-op", () => {
    expect(stepThreadStop(stops, "s3", 1)).toEqual(stops[2]);
    expect(stepThreadStop(stops, "s1", -1)).toEqual(stops[0]);
  });

  it("a stale current id (not in stops) re-seeds from the direction's end", () => {
    expect(stepThreadStop(stops, "gone", 1)).toEqual(stops[0]);
    expect(stepThreadStop(stops, "gone", -1)).toEqual(stops[2]);
  });

  it("empty stops returns null", () => {
    expect(stepThreadStop([], "s1", 1)).toBeNull();
    expect(stepThreadStop([], null, 1)).toBeNull();
  });
});
