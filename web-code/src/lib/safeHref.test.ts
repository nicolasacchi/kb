import { describe, expect, it } from "vitest";
import { safeHref } from "./safeHref";

describe("safeHref", () => {
  it("passes http and https through unchanged", () => {
    expect(safeHref("http://kb.example/a/x")).toBe("http://kb.example/a/x");
    expect(safeHref("https://kb.example/a/x?sec=kb-h-a")).toBe("https://kb.example/a/x?sec=kb-h-a");
  });

  it("rejects javascript:, data:, relative, empty and null", () => {
    expect(safeHref("javascript:alert(1)")).toBeNull();
    expect(safeHref("JaVaScRiPt:alert(1)")).toBeNull();
    expect(safeHref("data:text/html,<b>x</b>")).toBeNull();
    expect(safeHref("/a/kb/x.html")).toBeNull();
    expect(safeHref("//evil.example/x")).toBeNull();
    expect(safeHref("")).toBeNull();
    expect(safeHref("   ")).toBeNull();
    expect(safeHref(null)).toBeNull();
    expect(safeHref(undefined)).toBeNull();
  });
});
