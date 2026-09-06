import { describe, expect, it } from "vitest";
import {
  CAPTIONED_KINDS,
  captionsOn,
  groundednessOf,
  groundingTitle,
  parsePathRef,
  type Groundedness,
  type PathLens,
} from "./slateGrounding";
import { groundingKey } from "../hooks/useSlateGrounding";
import type { LineState, PathState } from "../api/doclens";

// D29 — the codelens -> caption mapping is a TABLE, so it is tested as one:
// every `path_state` x `line_state` pair kb-code can send has exactly one
// caption, and the only two that may say `grounded` are the ones where
// kb-code actually resolved something (invariant #2's "a wrong exact is a
// release blocker", read from the consuming side).

describe("parsePathRef", () => {
  it("splits a bare path", () => {
    expect(parsePathRef("path:crates/kb-core/src/slate.rs")).toEqual({
      path: "crates/kb-core/src/slate.rs",
      line: null,
    });
  });

  it("takes the FIRST line of a single, a range and a list", () => {
    expect(parsePathRef("path:a/b.rs:141")).toEqual({ path: "a/b.rs", line: 141 });
    expect(parsePathRef("path:a/b.rs:141-160")).toEqual({ path: "a/b.rs", line: 141 });
    expect(parsePathRef("path:a/b.rs:141,150,160")).toEqual({
      path: "a/b.rs",
      line: 141,
    });
  });

  it("is not fooled by a colon that belongs to the path", () => {
    expect(parsePathRef("path:weird:name.rs")).toEqual({
      path: "weird:name.rs",
      line: null,
    });
  });

  it("returns null for anything that is not a path: ref", () => {
    for (const raw of ["kb:docs/abc123", "post:#7", "session:8f2a", "path:", ""]) {
      expect(parsePathRef(raw), raw).toBeNull();
    }
  });

  it("refuses a zero/negative line rather than asking about it", () => {
    expect(parsePathRef("path:a/b.rs:0")).toEqual({ path: "a/b.rs", line: null });
  });
});

describe("groundednessOf — the whole codelens table", () => {
  // `lineHint` mirrors the wire's own `?line=` echo (SL7f): omitting the
  // 3rd arg means "no line was asked" (the bare-path shape), passing one
  // means "a line was asked, and `l` is what kb-code answered" — the two
  // must not collapse, since a bare path with no line asked is `grounded`
  // on the path alone regardless of `l`.
  const lens = (
    p: PathState | null,
    l?: LineState | null,
    lineHint?: number,
  ): PathLens => ({
    path_state: p,
    line_hint: lineHint ?? null,
    line_state: l ?? null,
  });

  const table: [PathLens | null | undefined, Groundedness, string][] = [
    [lens("present"), "grounded", "present, no line asked"],
    [lens("present", "confirmed", 12), "grounded", "present + confirmed = exact"],
    [
      lens("present", "drifted", 12),
      "ungrounded",
      "present + drifted = the CITED line number is stale, even though the token exists elsewhere",
    ],
    [
      lens("present", "absent", 12),
      "ungrounded",
      "no route mints this combo today (LineState::Absent never pairs with a present path); kept as a defensive default",
    ],
    [lens("present", "unverifiable", 12), "unknown", "kb-code declined to check"],
    [
      lens("present", null, 12),
      "unknown",
      "a line WAS asked but kb-code sent back no verdict at all — never optimistically grounded from a missing one",
    ],
    [lens("absent"), "ungrounded", "no such path"],
    [lens("absent", "confirmed", 12), "ungrounded", "path wins over any line"],
    [lens("ambiguous"), "unknown", "many candidates is not an answer"],
    [lens("external"), "unknown", "a vendored path this checkout does not own"],
    [lens(null), "unknown", "no path_state at all"],
    [null, "unknown", "no payload (a failed fetch)"],
    [undefined, "unknown", "nothing settled yet"],
  ];

  for (const [input, want, why] of table) {
    it(`${why} -> ${want}`, () => {
      expect(groundednessOf(input)).toBe(want);
    });
  }

  it("never says grounded for a path_state it does not know", () => {
    expect(groundednessOf({ path_state: "brand_new" as PathState })).toBe("unknown");
    // No `line_hint` given -> "no line was asked", so the bare-path grounded
    // arm applies regardless of the (irrelevant) `line_state` value.
    expect(
      groundednessOf({ path_state: "present", line_state: "brand_new" as LineState }),
    ).toBe("grounded");
  });

  it("a line WAS asked and line_state is a value this SPA does not know -> unknown", () => {
    expect(
      groundednessOf({
        path_state: "present",
        line_hint: 12,
        line_state: "brand_new" as LineState,
      }),
    ).toBe("unknown");
  });
});

describe("groundingKey — repo and context ride the query key (SL7f)", () => {
  it("includes the repo and, only when a line was asked, the context", () => {
    const withLine = groundingKey("platform", "alpha", { path: "a.rs", line: 12 }, "found path:a.rs:12 uses BATCH_SIZE");
    expect(withLine).toContain("alpha");
    expect(withLine).toContain("found path:a.rs:12 uses BATCH_SIZE");

    const bare = groundingKey("platform", "alpha", { path: "a.rs", line: null }, "found path:a.rs uses BATCH_SIZE");
    expect(bare).toContain("alpha");
    // A bare path ref never asks about a line, so its own line text is not
    // part of the question kb-code is asked — and must not affect the key.
    expect(bare).not.toContain("found path:a.rs uses BATCH_SIZE");
  });

  it("a missing repo does not collide with a real repo named the empty caption", () => {
    const noRepo = groundingKey("platform", null, { path: "a.rs", line: null }, null);
    const withRepo = groundingKey("platform", "alpha", { path: "a.rs", line: null }, null);
    expect(noRepo).not.toEqual(withRepo);
  });

  it("two cards citing the same (path, line) with DIFFERENT post text key separately", () => {
    const a = groundingKey("platform", "alpha", { path: "a.rs", line: 12 }, "text one");
    const b = groundingKey("platform", "alpha", { path: "a.rs", line: 12 }, "text two");
    expect(a).not.toEqual(b);
  });
});

describe("captionsOn", () => {
  it("captions knowledge cards only", () => {
    expect(CAPTIONED_KINDS).toEqual(["found", "tried"]);
    expect(captionsOn("found")).toBe(true);
    expect(captionsOn("tried")).toBe(true);
    for (const k of ["now", "warn", "take", "hand", "ask", "idea"] as const) {
      expect(captionsOn(k), k).toBe(false);
    }
  });
});

describe("groundingTitle", () => {
  it("names the daemon and never turns unknown into a verdict", () => {
    expect(groundingTitle("grounded")).toMatch(/kb-code resolved/);
    expect(groundingTitle("ungrounded")).toMatch(/did not find/);
    expect(groundingTitle("unknown")).toMatch(/unverified, not wrong/);
  });
});
