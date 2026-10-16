import { describe, it, expect } from "vitest";
import { renderToString } from "react-dom/server";
import CmdkCodeRow from "./CmdkCodeRow";

const noop = () => {};

describe("CmdkCodeRow", () => {
  it("renders a new-tab link to kb-code's search with the query shown", () => {
    const html = renderToString(
      <CmdkCodeRow
        href="https://kbc.example.com/search?q=theme"
        query=" theme "
        active={false}
        onHover={noop}
        onOpen={noop}
      />,
    );
    expect(html).toContain('href="https://kbc.example.com/search?q=theme"');
    expect(html).toContain('target="_blank"');
    expect(html).toContain('rel="noopener noreferrer"');
    expect(html).toContain("Search code for");
    expect(html).toContain("theme");
    expect(html).toContain("data-cmdk-code");
    expect(html).toContain('aria-selected="false"');
    expect(html).not.toContain("is-active");
  });

  it("marks the active cursor row", () => {
    const html = renderToString(
      <CmdkCodeRow href="http://x/search?q=a" query="a" active onHover={noop} onOpen={noop} />,
    );
    expect(html).toContain("is-active");
    expect(html).toContain('aria-selected="true"');
  });
});
