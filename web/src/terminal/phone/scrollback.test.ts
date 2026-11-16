import { describe, expect, it } from "vitest";
import { PHONE_SCROLLBACK_CAP, scrollbackFor } from "./scrollback";

describe("scrollbackFor", () => {
  it("caps the phone at 20000 lines", () => {
    expect(PHONE_SCROLLBACK_CAP).toBe(20_000);
    expect(scrollbackFor({ scrollback: 50_000 }, true)).toBe(20_000);
  });
  it("leaves the desktop profile untouched", () => {
    expect(scrollbackFor({ scrollback: 50_000 }, false)).toBe(50_000);
  });
  it("never raises a smaller profile on the phone", () => {
    expect(scrollbackFor({ scrollback: 5_000 }, true)).toBe(5_000);
  });
});
