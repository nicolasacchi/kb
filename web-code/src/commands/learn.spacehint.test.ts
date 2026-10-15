// OL-C11 — the Space hint is two key chips; the sentence lives in the tooltip.
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SPACE_HINT_TEXT, SpaceHint } from "./learn";
import { KBC_LEADER } from "./registry.gen";

describe("SpaceHint (compact)", () => {
  const html = renderToString(createElement(SpaceHint));

  it("renders the leader and ? as chips, with no prose", () => {
    expect(html).toContain("data-kbc-spacehint");
    expect(html.match(/<kbd>/g)?.length).toBe(2);
    expect(html).toContain(`<kbd>${KBC_LEADER}</kbd>`);
    expect(html).toContain("<kbd>?</kbd>");
    expect(html).not.toContain("for everything</");
    expect(html).not.toMatch(/>\s*for keys/);
  });

  it("carries the full sentence as its tooltip", () => {
    expect(SPACE_HINT_TEXT).toBe(`${KBC_LEADER} for everything · ? for keys`);
    expect(html).toContain(`title="${SPACE_HINT_TEXT}"`);
  });
});
