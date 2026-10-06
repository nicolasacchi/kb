import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { stickTopCss, stuckScrollDelta } from "./stickyHead";

describe("stickTopCss", () => {
  it("rounds a measured toolbar height up to whole px", () => {
    expect(stickTopCss(44)).toBe("44px");
    expect(stickTopCss(88.2)).toBe("89px");
  });
  it("returns null for an unusable measurement so the CSS fallback applies", () => {
    expect(stickTopCss(0)).toBeNull();
    expect(stickTopCss(-3)).toBeNull();
    expect(stickTopCss(Number.NaN)).toBeNull();
    expect(stickTopCss(null)).toBeNull();
    expect(stickTopCss(undefined)).toBeNull();
  });
});

describe("stuckScrollDelta", () => {
  it("is zero while the section header is still in normal flow", () => {
    expect(stuckScrollDelta(120, 44)).toBe(0);
    expect(stuckScrollDelta(44, 44)).toBe(0);
  });
  it("is the (negative) distance the scroller must move once the section scrolled past the pin line", () => {
    expect(stuckScrollDelta(-900, 44)).toBe(-944);
  });
});

describe("pinned file header CSS (v0.47 SH)", () => {
  const css = readFileSync(fileURLToPath(new URL("../styles/reviews.css", import.meta.url)), "utf-8");
  it("the section head pins at the measured toolbar height in BOTH map modes", () => {
    const head = css.match(/\.kbc-rdiff__section-head \{[^}]+\}/)?.[0] ?? "";
    expect(head).toContain("position: sticky");
    expect(head).toMatch(/top:\s*var\(--rdiff-stick-top,\s*var\(--topbar-h\)\);/);
    const mapped = css.match(/\.kbc-rdiff__body--mapped \.kbc-rdiff__section-head \{[^}]+\}/)?.[0] ?? "";
    expect(mapped).toBe("");
  });
  it("no overflow clipping ancestor between the sections and the scroller defeats sticky", () => {
    const split = css.match(/\.kbc-rdiff__split \{[^}]+\}/)?.[0] ?? "";
    expect(split).toMatch(/overflow-x:\s*clip\s*!important/);
    expect(split).toMatch(/overflow-y:\s*visible\s*!important/);
    expect(css).toMatch(/\.kbc-rdiff__split \[data-panel\][^{]*\{[^}]*overflow:\s*visible\s*!important/);
  });
});
