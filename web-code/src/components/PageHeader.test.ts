import { describe, expect, it } from "vitest";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import PageHeader from "./PageHeader";

// OL-P2 — a top-level <header> maps to the banner landmark and the Spine is
// already the page's one banner, so PageHeader must not emit one.
describe("PageHeader", () => {
  it("renders a div, never a header element", () => {
    const html = renderToString(
      createElement(PageHeader, { title: "Reviews", lede: "l", actions: "a", kicker: "k", className: "x" }),
    );
    expect(html).not.toContain("<header");
    expect(html).toContain('class="kbc-page-head x"');
    expect(html).toContain("<h1");
  });
});
