import { describe, expect, it } from "vitest";
import { parseUnifiedDiff } from "./diff";
import {
  HUNK_ID_SCHEMA,
  hunkFingerprintInput,
  hunkHasThreads,
  hunkId,
  hunkIds,
  hunkNewSpan,
  hunkOldSpan,
  hunkStats,
} from "./diffHunks";

/// The same change, at two different places in the file and with different
/// surrounding context — what a rebase produces.
const BASE = `diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -10,6 +10,7 @@ fn a() {
 ctx one
 ctx two
 ctx three
+added line
 ctx four
 ctx five
 ctx six
`;

const REBASED = `diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -840,6 +842,7 @@ fn a() {
 DIFFERENT ctx
 DIFFERENT ctx
 DIFFERENT ctx
+added line
 DIFFERENT ctx
 DIFFERENT ctx
 DIFFERENT ctx
`;

function firstHunk(text: string) {
  return parseUnifiedDiff(text).hunks[0];
}

describe("hunkId — the content address (kbc-hunkid/1)", () => {
  it("is stable and pinned", () => {
    // A GOLDEN. Changing this value means every already-stored
    // `review_hunk_viewed` row silently stops matching — see the module
    // header's bump rule. Do not "fix" it; bump the schema string.
    expect(HUNK_ID_SCHEMA).toBe("kbc-hunkid/1");
    expect(hunkId("src/lib.rs", firstHunk(BASE))).toBe("3ae1e11fec5881f1");
  });

  it("survives a rebase: line numbers and context are NOT hashed", () => {
    // The whole point. Same change, moved 830 lines down, with entirely
    // different surrounding context — same id.
    expect(hunkId("src/lib.rs", firstHunk(REBASED))).toBe(
      hunkId("src/lib.rs", firstHunk(BASE)),
    );
  });

  it("is per FILE — the same change in another file is another hunk", () => {
    expect(hunkId("src/other.rs", firstHunk(BASE))).not.toBe(
      hunkId("src/lib.rs", firstHunk(BASE)),
    );
  });

  it("changes when the changed lines change", () => {
    const other = firstHunk(BASE.replace("+added line", "+added LINE"));
    expect(hunkId("src/lib.rs", other)).not.toBe(hunkId("src/lib.rs", firstHunk(BASE)));
  });

  it("hashes the path plus the sigil-prefixed changed lines, and nothing else", () => {
    expect(hunkFingerprintInput("src/lib.rs", firstHunk(BASE))).toBe(
      "src/lib.rs\n+added line",
    );
  });

  it("is 16 lowercase hex characters", () => {
    expect(hunkId("a/b.rs", firstHunk(BASE))).toMatch(/^[0-9a-f]{16}$/);
  });

  it("hunkIds walks every hunk in order", () => {
    const parsed = parseUnifiedDiff(BASE);
    expect(hunkIds("src/lib.rs", parsed)).toEqual([hunkId("src/lib.rs", parsed.hunks[0])]);
  });
});

describe("hunk geometry", () => {
  it("counts additions and deletions", () => {
    expect(hunkStats(firstHunk(BASE))).toEqual({ additions: 1, deletions: 0 });
  });

  it("spans both sides", () => {
    expect(hunkNewSpan(firstHunk(BASE))).toEqual({ start: 10, end: 16 });
    expect(hunkOldSpan(firstHunk(BASE))).toEqual({ start: 10, end: 15 });
  });

  it("a pure-addition hunk has no old span", () => {
    const pureAdd = firstHunk(`@@ -0,0 +1,2 @@
+one
+two
`);
    expect(hunkOldSpan(pureAdd)).toBeNull();
    expect(hunkNewSpan(pureAdd)).toEqual({ start: 1, end: 2 });
  });
});

describe("hunkHasThreads", () => {
  const hunk = firstHunk(BASE);

  it("is false with no threads", () => {
    expect(hunkHasThreads(hunk, [])).toBe(false);
  });

  it("matches a new-side thread inside the span", () => {
    expect(hunkHasThreads(hunk, [{ side: "new", line: 13 }])).toBe(true);
  });

  it("does not match a new-side thread outside the span", () => {
    expect(hunkHasThreads(hunk, [{ side: "new", line: 900 }])).toBe(false);
  });

  it("an ORPHAN (no resolved line) belongs to no hunk", () => {
    // The honest answer: an orphan renders in the file's orphan section,
    // exactly as before diff v2.
    expect(hunkHasThreads(hunk, [{ side: "new", line: null }])).toBe(false);
  });

  it("a side-less thread is matched against BOTH spans (over-report, never hide)", () => {
    expect(hunkHasThreads(hunk, [{ side: null, line: 15 }])).toBe(true);
  });
});
