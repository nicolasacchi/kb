import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter, Route, Routes } from "react-router";
import { describe, expect, it } from "vitest";
import ReviewTabs from "./ReviewTabs";

function render(url: string): string {
  return renderToStaticMarkup(
    createElement(
      MemoryRouter,
      { initialEntries: [url] },
      createElement(
        Routes,
        null,
        createElement(Route, { path: "/r/:repo/*", element: createElement(ReviewTabs) }),
      ),
    ),
  );
}

/// Each opening `<a …>` tag, so attribute order never matters.
function anchors(html: string): string[] {
  return [...html.matchAll(/<a\b[^>]*>/g)].map((m) => m[0]);
}

describe("ReviewTabs", () => {
  it("is a nav named Review with the four tabs in order", () => {
    const html = render("/r/kb/~reviews");
    expect(html).toContain('<nav class="kbc-reviewtabs" aria-label="Review"');
    const keys = [...html.matchAll(/data-kbc-reviewtab="([a-z]+)"/g)].map((m) => m[1]);
    expect(keys).toEqual(["reviews", "prs", "branches", "compare"]);
  });

  it.each([
    ["/r/kb/~reviews", "reviews"],
    ["/r/kb/~reviews/12/diff", "reviews"],
    ["/r/kb/~prs", "prs"],
    ["/r/kb/~branches", "branches"],
    ["/r/kb/~compare?from=main&to=x", "compare"],
  ])("%s marks only %s aria-current", (url, key) => {
    const html = render(url);
    const current = anchors(html)
      .filter((a) => a.includes('aria-current="page"'))
      .map((a) => /data-kbc-reviewtab="([a-z]+)"/.exec(a)?.[1]);
    expect(current).toEqual([key]);
  });

  it("marks no tab on the range-diff page (it is in the family but not a tab)", () => {
    expect(render("/r/kb/~range-diff")).not.toContain('aria-current="page"');
  });
});
