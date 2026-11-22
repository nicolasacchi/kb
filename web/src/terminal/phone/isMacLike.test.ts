// FUMINPHONE (FUPHONELINKS follow-up) - which platforms open a link with Cmd
// (the hint on the desktop grid). Lives beside the phone link tests because the
// phone Read view shares `links.ts`.
import { describe, expect, it } from "vitest";
import { isMacLike, linkHint } from "../links";

describe("isMacLike", () => {
  it("is true for Apple platforms by `platform`", () => {
    for (const platform of ["MacIntel", "iPhone", "iPad", "iPod touch"]) {
      expect(isMacLike({ platform }), platform).toBe(true);
    }
  });

  it("is false for other platforms", () => {
    for (const platform of ["Linux armv8l", "Win32", "Linux x86_64"]) {
      expect(isMacLike({ platform }), platform).toBe(false);
    }
  });

  it("falls back to the user agent only when `platform` is empty", () => {
    expect(isMacLike({ platform: "", userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X)" })).toBe(true);
    expect(isMacLike({ userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0)" })).toBe(true);
    expect(isMacLike({ platform: "Linux armv8l", userAgent: "Mozilla/5.0 (Macintosh)" })).toBe(false);
  });

  it("is false without a navigator, and the hint follows it", () => {
    expect(isMacLike(undefined)).toBe(false);
    expect(isMacLike({})).toBe(false);
    expect(linkHint(true)).toBe("Cmd+click to open");
    expect(linkHint(false)).toBe("Ctrl+click to open");
  });
});
