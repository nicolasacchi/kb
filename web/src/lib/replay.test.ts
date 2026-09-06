import { describe, expect, test } from "vitest";
import {
  beatPathLabel,
  beatsForArtifact,
  formatElapsed,
  formatGap,
  groupSegments,
  kindGlyph,
  kindLabel,
  lineRangeLabel,
  playheadTarget,
  segmentIndexOf,
  stepSegment,
} from "./replay";
import type { ReplayBeatOut } from "../api/generated/ReplayBeatOut";
import type { ReplayKind } from "../api/generated/ReplayKind";

// ── fixtures ───────────────────────────────────────────────────────────────

let seq = 0;
function beat(
  kind: ReplayKind,
  detail: string,
  extra: Partial<ReplayBeatOut> & {
    path?: string;
    line_range?: [number, number];
    delta_secs?: number;
  } = {},
): ReplayBeatOut {
  const { path, line_range, delta_secs, ...resolution } = extra;
  return {
    beat: {
      seq: seq++,
      ts_unix: 1_700_000_000 + seq,
      delta_secs: delta_secs ?? 0,
      kind,
      detail,
      count: 1,
      ...(path !== undefined ? { path } : {}),
      ...(line_range !== undefined ? { line_range } : {}),
    },
    ...resolution,
  };
}

// ── Δt grammar (GOLDEN) ────────────────────────────────────────────────────

describe("formatGap — golden grammar", () => {
  // Every row here is the CONTRACT the scrubber stop labels and the rail
  // rows render. Changing one is a visible grammar change, not a refactor.
  const golden: [number, string][] = [
    [0, "+0s"],
    [1, "+1s"],
    [27, "+27s"],
    [59, "+59s"],
    [60, "+1m00s"],
    [64, "+1m04s"],
    [144, "+2m24s"],
    [3599, "+59m59s"],
    [3600, "+1h00m"],
    [3840, "+1h04m"],
    [86399, "+23h59m"],
    [86400, "+1d00h"],
    [183600, "+2d03h"],
  ];
  test.each(golden)("%d s → %s", (secs, want) => {
    expect(formatGap(secs)).toBe(want);
  });

  test("a backwards clock clamps to +0s rather than rendering a negative gap", () => {
    expect(formatGap(-1)).toBe("+0s");
    expect(formatGap(-99999)).toBe("+0s");
  });

  test("non-finite input degrades to +0s (never NaNs into the DOM)", () => {
    expect(formatGap(Number.NaN)).toBe("+0s");
    expect(formatGap(Number.POSITIVE_INFINITY)).toBe("+0s");
  });

  test("fractional seconds floor (a beat clock is whole seconds)", () => {
    expect(formatGap(27.9)).toBe("+27s");
  });

  test("formatElapsed is the same grammar without the + sign", () => {
    expect(formatElapsed(144)).toBe("2m24s");
    expect(formatElapsed(0)).toBe("0s");
  });
});

// ── segment grouping (GOLDEN) ──────────────────────────────────────────────

describe("groupSegments — golden grouping", () => {
  test("a prompt opens a segment; following beats belong to it", () => {
    const beats = [
      beat("prompt", "add the replay route"),
      beat("assistant", "I'll start by reading the wire"),
      beat("read", "sessions.rs", { path: "/src/sessions.rs" }),
      beat("prompt", "now wire the keyboard"),
      beat("edit", "keymap.ts", { path: "/web/src/lib/keymap.ts" }),
    ];
    const segs = groupSegments(beats);
    expect(segs).toHaveLength(2);
    expect(segs[0].title).toBe("add the replay route");
    expect(segs[0].startIndex).toBe(0);
    expect(segs[0].beats).toHaveLength(3);
    expect(segs[0].prompt).toBe(beats[0]);
    expect(segs[1].title).toBe("now wire the keyboard");
    expect(segs[1].startIndex).toBe(3);
    expect(segs[1].beats).toHaveLength(2);
    expect(segs.map((s) => s.index)).toEqual([0, 1]);
  });

  test("beats before the first prompt keep their own promptless segment", () => {
    const beats = [
      beat("read", "a.html", { path: "/c/a.html" }),
      beat("bash", "git status"),
      beat("prompt", "carry on"),
    ];
    const segs = groupSegments(beats);
    expect(segs).toHaveLength(2);
    expect(segs[0].prompt).toBeNull();
    expect(segs[0].title).toBe("before the first prompt");
    expect(segs[0].beats).toHaveLength(2);
    expect(segs[1].prompt).not.toBeNull();
  });

  test("two prompts in a row each get a segment (an empty one is still a beat)", () => {
    const segs = groupSegments([beat("prompt", "one"), beat("prompt", "two")]);
    expect(segs.map((s) => s.beats.length)).toEqual([1, 1]);
  });

  test("concatenating the segments recovers the flat array exactly (nothing dropped, nothing reordered)", () => {
    const beats = [
      beat("assistant", "resuming"),
      beat("prompt", "p1"),
      beat("read", "x", { path: "/c/x.html" }),
      beat("prompt", "p2"),
      beat("commit", "feat: thing"),
      beat("other", "TodoWrite"),
    ];
    const flat = groupSegments(beats).flatMap((s) => s.beats);
    expect(flat).toEqual(beats);
  });

  test("an empty timeline groups to no segments", () => {
    expect(groupSegments([])).toEqual([]);
  });
});

describe("segmentIndexOf / stepSegment", () => {
  const beats = [
    beat("prompt", "p1"), // 0
    beat("read", "a", { path: "/c/a.html" }), // 1
    beat("prompt", "p2"), // 2
    beat("edit", "b", { path: "/c/b.html" }), // 3
    beat("prompt", "p3"), // 4
  ];
  const segs = groupSegments(beats);

  test("maps a flat index onto its segment", () => {
    expect(segmentIndexOf(segs, 0)).toBe(0);
    expect(segmentIndexOf(segs, 1)).toBe(0);
    expect(segmentIndexOf(segs, 2)).toBe(1);
    expect(segmentIndexOf(segs, 4)).toBe(2);
    expect(segmentIndexOf([], 0)).toBe(-1);
  });

  test("] lands on the next segment's first beat and stops at the last", () => {
    expect(stepSegment(segs, 0, 1)).toBe(2);
    expect(stepSegment(segs, 1, 1)).toBe(2);
    expect(stepSegment(segs, 4, 1)).toBe(4);
  });

  test("[ rewinds to the segment head first, then to the previous segment", () => {
    expect(stepSegment(segs, 3, -1)).toBe(2);
    expect(stepSegment(segs, 2, -1)).toBe(0);
    expect(stepSegment(segs, 0, -1)).toBe(0);
  });
});

// ── selectors ──────────────────────────────────────────────────────────────

describe("beatsForArtifact", () => {
  const beats = [
    beat("read", "a", { path: "/c/a.html", kb: "canon", artifact_id: "aaa" }),
    beat("read", "b", { path: "/c/b.html", kb: "canon", artifact_id: "bbb" }),
    beat("edit", "a", { path: "/c/a.html", kb: "canon", artifact_id: "aaa" }),
    beat("bash", "ls"),
  ];

  test("keeps only the beats resolved to that artifact", () => {
    expect(beatsForArtifact(beats, "aaa").map((b) => b.beat.detail)).toEqual([
      "a",
      "a",
    ]);
  });

  test("an absent id selects nothing (never everything)", () => {
    expect(beatsForArtifact(beats, null)).toEqual([]);
    expect(beatsForArtifact(beats, undefined)).toEqual([]);
    expect(beatsForArtifact(beats, "")).toEqual([]);
  });
});

describe("playheadTarget — walks backward to the most recent showable beat", () => {
  const beats = [
    beat("prompt", "p"), // 0 — nothing showable
    beat("read", "a", {
      path: "/c/a.html",
      kb: "canon",
      artifact_id: "aaa",
      source_relative: "a.html",
      line_range: [10, 20],
      heading_slug: "kb-h-intro",
    }), // 1
    beat("assistant", "thinking about it"), // 2
    beat("bash", "cargo test"), // 3
    beat("read", "lib.rs", { path: "/src/lib.rs" }), // 4 — unresolved
    beat("assistant", "done"), // 5
  ];

  test("an artifact beat resolves to the artifact, slug included", () => {
    const t = playheadTarget(beats, 1);
    expect(t).toEqual({
      kind: "artifact",
      kb: "canon",
      artifactId: "aaa",
      sourceRelative: "a.html",
      slug: "kb-h-intro",
      fromIndex: 1,
    });
  });

  test("a non-file beat keeps the last artifact in play (not a blank pane)", () => {
    const t = playheadTarget(beats, 3);
    expect(t?.kind).toBe("artifact");
    if (t?.kind !== "artifact") throw new Error("unreachable");
    expect(t.artifactId).toBe("aaa");
    // …and it reports which beat it came from, so the UI never claims the
    // current beat touched the file.
    expect(t.fromIndex).toBe(1);
  });

  test("an unresolved path renders as a plain path, never dropped", () => {
    expect(playheadTarget(beats, 4)).toEqual({
      kind: "path",
      path: "/src/lib.rs",
      fromIndex: 4,
    });
    // …and it stays in play for the beats after it.
    expect(playheadTarget(beats, 5)).toEqual({
      kind: "path",
      path: "/src/lib.rs",
      fromIndex: 4,
    });
  });

  test("nothing showable yet → null", () => {
    expect(playheadTarget(beats, 0)).toBeNull();
    expect(playheadTarget([], 0)).toBeNull();
  });

  test("an out-of-range index clamps into the timeline", () => {
    expect(playheadTarget(beats, 999)?.fromIndex).toBe(4);
    expect(playheadTarget(beats, -5)).toBeNull();
  });
});

// ── labels ─────────────────────────────────────────────────────────────────

describe("labels", () => {
  test("every ReplayKind has a glyph and a label", () => {
    const kinds: ReplayKind[] = [
      "prompt",
      "assistant",
      "read",
      "edit",
      "write",
      "bash",
      "commit",
      "decision",
      "search",
      "subagent",
      "other",
    ];
    for (const k of kinds) {
      expect(kindGlyph(k).length).toBeGreaterThan(0);
      expect(kindLabel(k).length).toBeGreaterThan(0);
    }
  });

  test("beatPathLabel prefers the resolved source-relative path, else the raw one", () => {
    expect(
      beatPathLabel(
        beat("read", "a", { path: "/abs/a.html", source_relative: "a.html" }),
      ),
    ).toBe("a.html");
    expect(beatPathLabel(beat("read", "a", { path: "/etc/hosts" }))).toBe(
      "/etc/hosts",
    );
    expect(beatPathLabel(beat("prompt", "hi"))).toBeNull();
  });

  test("lineRangeLabel is null when the transcript carried no range (never invented)", () => {
    expect(lineRangeLabel(beat("read", "a", { path: "/c/a.html" }))).toBeNull();
    expect(
      lineRangeLabel(beat("read", "a", { path: "/c/a.html", line_range: [3, 9] })),
    ).toBe("L3–9");
    expect(
      lineRangeLabel(beat("read", "a", { path: "/c/a.html", line_range: [7, 7] })),
    ).toBe("L7");
  });
});
