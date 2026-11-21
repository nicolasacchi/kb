import { describe, expect, it } from "vitest";
import { chipText, ownerClass, ownerLabel } from "./gridOwner";

describe("grid owner words", () => {
  it("classifies every owner class", () => {
    expect(ownerClass(null)).toBe("free");
    expect(ownerClass(undefined)).toBe("free");
    expect(ownerClass("")).toBe("free");
    expect(ownerClass("kb-term")).toBe("web");
    expect(ownerClass("local:4242")).toBe("local");
    expect(ownerClass("desk-1")).toBe("other");
    // The prefix must be followed by the colon: this is not a local attach.
    expect(ownerClass("localhost")).toBe("other");
  });

  it("chip text for every owner class, never 'this window'", () => {
    const g = { cols: 201, rows: 48 };
    expect(chipText(g, "kb-term")).toBe("201x48 · web");
    expect(chipText(g, "local:9")).toBe("201x48 · local attach");
    expect(chipText(g, null)).toBe("201x48 · free");
    expect(chipText(g, "desk-1")).toBe("201x48 · desk-1");
    expect(chipText(g, undefined)).toBe("201x48");
    for (const o of ["kb-term", "local:9", null, "desk-1"]) expect(ownerLabel(o)).not.toMatch(/window/i);
  });
});
