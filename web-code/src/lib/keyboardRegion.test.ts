import { describe, expect, it } from "vitest";
import { nextKeyboardRegion, type RegionTarget } from "./keyboardRegion";

/// A minimal `closest()`-only stand-in for a `focusin` event's target —
/// `nextKeyboardRegion` only ever calls `.closest(selector)`, so this is
/// the whole surface a unit test needs to fake.
function targetIn(...selectors: string[]): RegionTarget {
  return {
    closest: (selector: string) => (selectors.includes(selector) ? {} : null),
  };
}

describe("nextKeyboardRegion", () => {
  it("moves to buffer when the focusin target is inside the CM6 buffer", () => {
    expect(nextKeyboardRegion("tree", targetIn(".kbc-codeview"))).toBe("buffer");
    // Already buffer — stays buffer (idempotent, not just a toggle).
    expect(nextKeyboardRegion("buffer", targetIn(".kbc-codeview"))).toBe("buffer");
  });

  it("moves to tree when the focusin target is inside the file tree", () => {
    expect(nextKeyboardRegion("buffer", targetIn("[data-kbc-tree]"))).toBe("tree");
    expect(nextKeyboardRegion("tree", targetIn("[data-kbc-tree]"))).toBe("tree");
  });

  it("never regresses on an unnamed target — the exact case that used to strand the region", () => {
    // A `focusin` landing on <body> (the DOM spec's silent fallback when a
    // blur() hands off to nothing) names neither surface — this is the
    // "nothing to infer from" case `nextKeyboardRegion` must leave alone,
    // not overwrite with a guess.
    const body = targetIn(); // matches no selector
    expect(nextKeyboardRegion("tree", body)).toBe("tree");
    expect(nextKeyboardRegion("buffer", body)).toBe("buffer");
  });

  it("never regresses on a target that names neither surface", () => {
    const elsewhere = targetIn(".some-unrelated-toolbar-button");
    expect(nextKeyboardRegion("tree", elsewhere)).toBe("tree");
    expect(nextKeyboardRegion("buffer", elsewhere)).toBe("buffer");
  });

  it("never regresses on a null target", () => {
    expect(nextKeyboardRegion("tree", null)).toBe("tree");
    expect(nextKeyboardRegion("buffer", null)).toBe("buffer");
  });

  // `Ctrl-w h`'s scope switch itself (`Reader.tsx`'s `handlePaneFocus`) is
  // a direct, synchronous `setKeyboardRegion("tree")` call — not a
  // derivation through this function at all, by design (see this
  // module's doc: the whole point is that the scope-owning WRITE no
  // longer depends on any DOM focus event landing). That direct write is
  // exercised by `split.spec.ts`'s "Shift+Enter in the tree opens the
  // focused row into pane2" e2e case, not re-tested here as a unit —
  // there is no branching logic in a bare `setState("tree")` call worth a
  // dedicated reducer test.
});
