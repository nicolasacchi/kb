import { describe, expect, it } from "vitest";
import { setsUrl, setUrl, tourUrl } from "./setsUrl";

// Golden tables, same discipline as `codeUrl.test.ts`'s own — one table,
// exact strings, no snapshot fuzziness.

describe("setsUrl", () => {
  it("builds the repo-scoped sets list URL", () => {
    expect(setsUrl("kb")).toBe("/r/kb/~sets");
  });

  it("encodes a repo name needing it", () => {
    expect(setsUrl("my repo")).toBe("/r/my%20repo/~sets");
  });
});

describe("setUrl", () => {
  it("builds one set's detail URL", () => {
    expect(setUrl("kb", "set_abc123")).toBe("/r/kb/~sets/set_abc123");
  });

  it("encodes an id needing it", () => {
    expect(setUrl("kb", "set with space")).toBe("/r/kb/~sets/set%20with%20space");
  });

  it("encodes a repo name needing it", () => {
    expect(setUrl("my repo", "set_1")).toBe("/r/my%20repo/~sets/set_1");
  });
});

describe("tourUrl", () => {
  it("builds the tour URL with no ?step=", () => {
    expect(tourUrl("kb", "set_1")).toBe("/r/kb/~sets/set_1/~tour");
  });

  it("appends ?step= when given", () => {
    expect(tourUrl("kb", "set_1", 3)).toBe("/r/kb/~sets/set_1/~tour?step=3");
  });

  it("omits ?step= for a non-positive step", () => {
    expect(tourUrl("kb", "set_1", 0)).toBe("/r/kb/~sets/set_1/~tour");
    expect(tourUrl("kb", "set_1", -1)).toBe("/r/kb/~sets/set_1/~tour");
  });

  it("encodes repo/id like setUrl", () => {
    expect(tourUrl("my repo", "set with space", 2)).toBe(
      "/r/my%20repo/~sets/set%20with%20space/~tour?step=2",
    );
  });
});
