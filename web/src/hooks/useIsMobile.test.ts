import { describe, expect, it } from "vitest";
import { MOBILE_MAX_WIDTH } from "./useIsMobile";

// M1 — the mobile breakpoint is duplicated by necessity: CSS media queries
// can't read a JS constant or custom property. Pin the JS side so any change is
// deliberate; the `@media (max-width: 860px)` rules in styles/mobile.css must be
// updated in lock-step (both files carry cross-referencing comment headers).
// (A cross-file CSS read isn't viable here — vitest's node env stubs CSS imports
// and the SPA tsconfig has no node types — so the literal is pinned on this side
// and the responsive e2e exercises the breakpoint behaviour end-to-end.)
describe("mobile breakpoint", () => {
  it("MOBILE_MAX_WIDTH is the documented 860px shared with mobile.css", () => {
    expect(MOBILE_MAX_WIDTH).toBe(860);
  });
});
