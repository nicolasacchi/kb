import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { CrossingChip } from "./CrossingChip";

describe("CrossingChip", () => {
  it("renders an anchor with rel=noreferrer for href", () => {
    const html = renderToString(
      <CrossingChip label="Cited by 1 doc" count={1} href="https://example.test/a" />,
    );
    expect(html.startsWith("<a ")).toBe(true);
    expect(html).toContain('href="https://example.test/a"');
    expect(html).toContain('rel="noreferrer"');
    expect(html).toContain('aria-label="Cited by 1 doc"');
    expect(html).toContain('data-testid="crossing-chip"');
  });

  it("opens in a new tab only when asked", () => {
    const on = renderToString(<CrossingChip label="x" href="https://example.test/" newTab />);
    expect(on).toContain('target="_blank"');
    const off = renderToString(<CrossingChip label="x" href="https://example.test/" />);
    expect(off).not.toContain("target=");
  });

  it("renders a button for onPress", () => {
    const html = renderToString(<CrossingChip label="Cited by 2 docs" onPress={() => {}} />);
    expect(html.startsWith("<button ")).toBe(true);
    expect(html).toContain('type="button"');
    expect(html).not.toContain("disabled");
    expect(html).not.toContain("<a ");
  });

  it("renders a disabled button carrying the title when neither is given", () => {
    const html = renderToString(<CrossingChip label="Cited by 1 doc" title="no destination" />);
    expect(html.startsWith("<button ")).toBe(true);
    expect(html).toContain("disabled");
    expect(html).toContain('title="no destination"');
  });

  it("honours ariaLabel and testId, and carries tone as an attribute and in text", () => {
    const html = renderToString(
      <CrossingChip
        label="Cited by 1 doc - path no longer present"
        ariaLabel="Cited by one doc"
        testId="x-chip"
        tone="warn"
        href="https://example.test/"
      />,
    );
    expect(html).toContain('aria-label="Cited by one doc"');
    expect(html).toContain('data-testid="x-chip"');
    expect(html).toContain('data-tone="warn"');
    expect(html).toContain("path no longer present");
  });
});
