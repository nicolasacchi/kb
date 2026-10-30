import { renderToString } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { CrossingChip } from "./CrossingChip";

// Node's fs via getBuiltinModule so `tsc -b` needs no Node typings (see Spine.test.tsx).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(url: URL, enc: "utf8"): string;
};

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

  it("keeps a 44px touch target and the icon+count collapse at the mobile breakpoint", () => {
    const css = fs.readFileSync(new URL("./crossingchip.css", import.meta.url), "utf8");
    const start = css.indexOf("@media (max-width: 860px)");
    expect(start).toBeGreaterThan(-1);
    const block = css.slice(start);
    expect(block).toMatch(/\.kb-crossing\s*\{[^}]*min-width:\s*44px/);
    expect(block).toMatch(/\.kb-crossing\s*\{[^}]*min-height:\s*44px/);
    expect(block).toMatch(/\.kb-crossing__label\s*\{[^}]*display:\s*none/);
  });
});
