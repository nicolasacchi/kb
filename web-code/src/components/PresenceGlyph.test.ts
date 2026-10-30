import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { PresenceGlyph } from "./PresenceGlyph";

describe("PresenceGlyph", () => {
  it("exposes the claim as real text, not only an aria-hidden glyph", () => {
    const html = renderToString(
      createElement(PresenceGlyph, { label: "Probably claude session 3f2a, changed 20 s ago" }),
    );
    expect(html).toContain('class="kbc-sr-only"');
    expect(html).toContain("Probably claude session 3f2a, changed 20 s ago");
    expect(html).toMatch(/class="kbc-tree__presence" aria-hidden="true"/);
  });
  it("shows the roll-up count on the visible glyph", () => {
    const html = renderToString(createElement(PresenceGlyph, { label: "2 changed files inside", count: 2 }));
    expect(html).toContain("kbc-tree__presence-count");
  });
});
