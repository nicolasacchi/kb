import { describe, expect, it } from "vitest";
import type { LaneSection } from "../api/types";
import { HEADER_ROW } from "./paletteReducer";
import { resolveSearchTarget } from "./searchTargets";

function section(lane: LaneSection["lane"], results: unknown[]): LaneSection {
  return { lane, results, truncated: false };
}

const SECTIONS: LaneSection[] = [
  section("files", [{ repo: "kb", path: "src/lib.rs", score: 12 }]),
  section("symbols", [
    { repo: "kb", path: "src/lib.rs", score: 5, ordinal: 0, name: "add", kind: "function", line_start: 3, line_end: 5, col_start: 0, col_end: 1, container: null, signature: null },
  ]),
  section("text", [{ path: "src/lib.rs", matches: [{ line_no: 4, line: "a + b", byte_range: [0, 1] }] }]),
  section("semantic", [{ repo: "kb", path: "src/lib.rs", span_start: 3, span_end: 6, score: 0.8, snippet: "fn add" }]),
  section("sessions", [{ session_id: "sess-1", title: "fixed the bug", score: 1, started_at: 0, source: "kb digests" }]),
  section("transcripts", [
    { session_id: "sess-1", uuid: "u-1", ts: 0, kind: "assistant", tool_name: null, project_dir: "/tmp", snippet: "hi", is_sidechain: false },
  ]),
];

describe("resolveSearchTarget", () => {
  it("resolves the header row to full-search regardless of lane", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 0, row: HEADER_ROW }, undefined, "q")).toEqual({
      kind: "full-search",
    });
  });

  it("resolves a files hit to the reader at its own repo/path", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 0, row: 0 }, undefined, "q")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "src/lib.rs",
    });
  });

  it("resolves a symbols hit to the reader at line_start", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 1, row: 0 }, undefined, "q")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "src/lib.rs",
      line: 3,
    });
  });

  it("resolves a text hit using the fallback repo when no repo: filter is typed", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 2, row: 0 }, "kb", "needle")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "src/lib.rs",
      line: 4,
    });
  });

  it("resolves a text hit's repo from a repo: filter over the fallback repo", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 2, row: 0 }, "other", "needle repo:kb")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "src/lib.rs",
      line: 4,
    });
  });

  it("returns null for a text hit with no resolvable repo at all", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 2, row: 0 }, undefined, "needle")).toBeNull();
  });

  it("resolves a semantic hit to the reader at span_start", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 3, row: 0 }, undefined, "q")).toEqual({
      kind: "reader",
      repo: "kb",
      path: "src/lib.rs",
      line: 3,
    });
  });

  it("resolves a sessions hit to an external kb SPA link", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 4, row: 0 }, undefined, "q")).toEqual({
      kind: "external",
      href: "http://127.0.0.1:4000/sessions/sess-1",
    });
  });

  it("resolves a transcripts hit to a popover (no file/path field today)", () => {
    const target = resolveSearchTarget(SECTIONS, { section: 5, row: 0 }, undefined, "q");
    expect(target?.kind).toBe("popover");
    if (target?.kind === "popover") {
      expect(target.hit.uuid).toBe("u-1");
    }
  });

  it("returns null for an out-of-range row", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 0, row: 7 }, undefined, "q")).toBeNull();
  });

  it("returns null for an out-of-range section", () => {
    expect(resolveSearchTarget(SECTIONS, { section: 9, row: 0 }, undefined, "q")).toBeNull();
  });

  it("returns null when there are no sections at all (no section to resolve a header for)", () => {
    expect(resolveSearchTarget([], { section: 0, row: HEADER_ROW }, undefined, "q")).toBeNull();
    expect(resolveSearchTarget([], { section: 0, row: 0 }, undefined, "q")).toBeNull();
  });
});
