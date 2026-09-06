import { describe, expect, it } from "vitest";
import {
  layerDiffHeader,
  layerTags,
  pathBasename,
  stacksUrl,
} from "./stacksFormat";
import type { StackLayer } from "../api/types";

function layer(over: Partial<StackLayer> = {}): StackLayer {
  return {
    branch: "feature-x-2",
    base: "feature-x",
    ahead: 1,
    behind: 0,
    stale: false,
    tip_shared: false,
    unresolved: false,
    tip: { sha: "abcdef0123456789", subject: "stack layer", date: 0 },
    ...over,
  };
}

describe("stacksUrl", () => {
  it("builds bare sentinel", () => {
    expect(stacksUrl("kb")).toBe("/r/kb/~stacks");
  });

  it("adds all + branch", () => {
    expect(stacksUrl("kb", { all: true, branch: "feature-x-2" })).toBe(
      "/r/kb/~stacks?all=1&branch=feature-x-2",
    );
  });
});

describe("layerTags", () => {
  it("emits neutral stale/unresolved labels", () => {
    const tags = layerTags(layer({ stale: true, unresolved: true, tip_shared: true }));
    expect(tags.map((t) => t.label)).toEqual([
      "base moved",
      "tip shared",
      "walk bound exceeded",
    ]);
  });

  it("returns empty when clean", () => {
    expect(layerTags(layer())).toEqual([]);
  });
});

describe("layerDiffHeader", () => {
  it("formats base + short tip", () => {
    expect(layerDiffHeader("feature-x", "abcdef0123456789", false)).toMatch(
      /^diff vs base feature-x @ abcdef0/,
    );
  });

  it("annotates stale", () => {
    expect(layerDiffHeader("main", "1234567890abcdef", true)).toContain(
      "base moved since cut",
    );
  });
});

describe("pathBasename", () => {
  it("takes the last segment", () => {
    expect(pathBasename("src/lib/foo.rs")).toBe("foo.rs");
    expect(pathBasename("foo.rs")).toBe("foo.rs");
  });
});
