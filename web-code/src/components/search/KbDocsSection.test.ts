import { describe, it, expect } from "vitest";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import KbDocsSection from "./KbDocsSection";
import type { KbSearchOut } from "../../api/types";

const noop = () => {};

interface Props {
  response: KbSearchOut | null;
  loading: boolean;
  laneIndex: number;
  cursor: { section: number; row: number };
}
const render = (p: Props) => renderToString(createElement(KbDocsSection, { ...p, onHover: noop }));
const cursor = { section: 3, row: -1 };

const res = (over: Partial<KbSearchOut> = {}): KbSearchOut => ({
  hits: [
    {
      kb: "platform",
      id: "abc",
      title: "Theme notes",
      url: "https://kb.example.com/a/platform/design/theme%20notes.html",
      snippet: "the palette family",
    },
  ],
  kb_public_url: "https://kb.example.com",
  ...over,
});

describe("KbDocsSection", () => {
  it("renders hits as new-tab anchors to the kb permalink under a Documents header", () => {
    const html = render({ response: res(), loading: false, laneIndex: 3, cursor: cursor });
    expect(html).toContain("Documents");
    expect(html).toContain("data-kbc-docs-lane");
    expect(html).toContain('href="https://kb.example.com/a/platform/design/theme%20notes.html"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain("Theme notes");
    expect(html).toContain("platform");
    expect(html).toContain("design/theme notes.html");
    // it is NOT one of the daemon's six lanes the e2e suite counts
    expect(html).not.toContain("data-kbc-lane=");
  });

  it("marks the cursor row and the header", () => {
    const row = render({ response: res(), loading: false, laneIndex: 3, cursor: { section: 3, row: 0 } });
    expect(row).toContain('aria-selected="true"');
    const header = render({ response: res(), loading: false, laneIndex: 3, cursor: cursor });
    expect(header).toContain("kbc-search__header is-active");
  });

  it.each([
    ["kb unreachable", "unreachable"],
    ["kb disabled", "not configured"],
    ["kb_sibling_mismatch", "sibling contract"],
  ])("an unavailable '%s' response is one muted line, no rows, no alert", (unavailable, words) => {
    const html = render({ response: res({ hits: [], unavailable }), loading: false, laneIndex: 0, cursor: { section: 9, row: 0 } });
    expect(html).toContain("kbc-search__note");
    expect(html).toContain(`data-kbc-docs-unavailable="${unavailable}"`);
    expect(html).toContain(words);
    expect(html).not.toContain("data-kbc-docs-row");
    expect(html).not.toContain('role="alert"');
  });

  it("shows a skeleton before the first answer and 'No matches' on an empty one", () => {
    const loading = render({ response: null, loading: true, laneIndex: 0, cursor: cursor });
    expect(loading).toContain("Searching");
    const empty = render({ response: res({ hits: [] }), loading: false, laneIndex: 0, cursor: cursor });
    expect(empty).toContain("No matches");
  });

  it("refuses to link a non-http(s) permalink", () => {
    const html = render({
      response: res({
        hits: [{ kb: "k", id: "1", title: "Bad", url: "javascript:alert(1)", snippet: null }],
      }),
      loading: false,
      laneIndex: 0,
      cursor,
    });
    expect(html).toContain("Bad");
    expect(html).not.toContain("javascript:");
    expect(html).not.toContain("<a ");
  });
});
