import { describe, expect, it } from "vitest";
import { DISPOSITIONS, nextDispositionClick } from "./DispositionMenu";

describe("nextDispositionClick", () => {
  it("sets the disposition when nothing is currently set", () => {
    expect(nextDispositionClick(null, "agree")).toBe("agree");
    expect(nextDispositionClick(undefined, "waive")).toBe("waive");
  });

  it("clears the disposition when clicking the already-active one", () => {
    expect(nextDispositionClick("agree", "agree")).toBeNull();
  });

  it("switches to a different disposition when a different one is clicked", () => {
    expect(nextDispositionClick("agree", "dispute")).toBe("dispute");
  });

  it("is idempotent: clicking the cleared state's disposition again re-sets it", () => {
    const afterFirstClick = nextDispositionClick(null, "waive"); // "waive"
    const afterSecondClick = nextDispositionClick(afterFirstClick, "waive"); // null
    const afterThirdClick = nextDispositionClick(afterSecondClick, "waive"); // "waive"
    expect([afterFirstClick, afterSecondClick, afterThirdClick]).toEqual(["waive", null, "waive"]);
  });
});

describe("DISPOSITIONS", () => {
  it("is exactly the server's 4-value vocab, in a stable order", () => {
    expect(DISPOSITIONS.map((d) => d.key)).toEqual(["agree", "dispute", "waive", "fix-later"]);
  });

  it("never includes the mock's invented fixed/follow-up states", () => {
    const keys = DISPOSITIONS.map((d) => d.key as string);
    expect(keys).not.toContain("fixed");
    expect(keys).not.toContain("follow-up");
  });
});
