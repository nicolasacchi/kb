import { describe, expect, it } from "vitest";
import { parseDeskParam } from "./deskParam";

// `parseDeskParam` is a pure function of the URL's `search` string — no
// Router context needed (same discipline as `useActiveRepo.test.ts`'s
// `repoOf`). Golden-style: every branch of the grammar gets its own case.
describe("parseDeskParam (V70-A0 — the ?desk= override contract)", () => {
  it("parses each named preset", () => {
    expect(parseDeskParam("?desk=read")).toBe("read");
    expect(parseDeskParam("?desk=review")).toBe("review");
    expect(parseDeskParam("?desk=explore")).toBe("explore");
    expect(parseDeskParam("?desk=present")).toBe("present");
  });

  it("parses the legacy escape hatch distinctly from a preset", () => {
    expect(parseDeskParam("?desk=legacy")).toBe("legacy");
  });

  it("is null when the param is absent", () => {
    expect(parseDeskParam("")).toBeNull();
    expect(parseDeskParam("?ref=main")).toBeNull();
  });

  it("is null on an empty value", () => {
    expect(parseDeskParam("?desk=")).toBeNull();
  });

  it("is null on an unknown preset — never a silent default", () => {
    expect(parseDeskParam("?desk=bogus")).toBeNull();
    expect(parseDeskParam("?desk=Read")).toBeNull(); // case-sensitive
    expect(parseDeskParam("?desk=legacyish")).toBeNull();
  });

  it("reads desk alongside other params, in any position", () => {
    expect(parseDeskParam("?ref=main&desk=review&line=10")).toBe("review");
    expect(parseDeskParam("?desk=explore&pane2=x.rs")).toBe("explore");
  });

  it("takes the first value on a repeated param (URLSearchParams.get semantics)", () => {
    expect(parseDeskParam("?desk=read&desk=present")).toBe("read");
  });
});
