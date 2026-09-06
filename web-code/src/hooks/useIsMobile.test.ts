import { describe, expect, it } from "vitest";
import { MOBILE_MAX_WIDTH } from "./useIsMobile";

// F5 — the mobile breakpoint is duplicated by necessity: CSS media queries
// can't read a JS constant or custom property. Pin the JS side so any change
// is deliberate; the `@media (max-width: 860px)` rules in styles/mobile.css
// must be updated in lock-step (both files carry cross-referencing comment
// headers). Mirrors kb's own `web/src/hooks/useIsMobile.test.ts` 1:1 — this
// crate's vitest config runs in `environment: "node"` (no jsdom/matchMedia),
// same reasoning as kb's own test: the responsive e2e (`e2e/mobile.spec.ts`)
// is what exercises the breakpoint's actual behaviour in a browser.
describe("mobile breakpoint", () => {
  it("MOBILE_MAX_WIDTH is the documented 860px shared with mobile.css", () => {
    expect(MOBILE_MAX_WIDTH).toBe(860);
  });
});
