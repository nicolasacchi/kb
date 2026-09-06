import { describe, expect, it } from "vitest";
import { PREFIX_CHIPS, applyPrefixChip, stripLeadingPrefix } from "./prefixChips";

describe("PREFIX_CHIPS", () => {
  it("has exactly the six lane-selecting prefixes grammar.rs documents", () => {
    expect(PREFIX_CHIPS.map((c) => c.label)).toEqual(["@", "#", "/", "?", "~", "~~"]);
  });
});

describe("stripLeadingPrefix", () => {
  it("strips each single-char prefix", () => {
    expect(stripLeadingPrefix("@Widget")).toBe("Widget");
    expect(stripLeadingPrefix("#config.rs")).toBe("config.rs");
    expect(stripLeadingPrefix("/needle")).toBe("needle");
    expect(stripLeadingPrefix("~fixed a bug")).toBe("fixed a bug");
  });

  it("strips the double-tilde prefix whole, not as a leftover single tilde", () => {
    expect(stripLeadingPrefix("~~gorgonzola")).toBe("gorgonzola");
  });

  it("strips the ?nl prefix with its trailing space", () => {
    expect(stripLeadingPrefix("?nl how does auth work")).toBe("how does auth work");
  });

  it("strips a bare ?nl with nothing after it", () => {
    expect(stripLeadingPrefix("?nl")).toBe("");
  });

  it("leaves a query with no recognised prefix untouched", () => {
    expect(stripLeadingPrefix("plain query")).toBe("plain query");
    expect(stripLeadingPrefix("?nlnotarealprefix")).toBe("?nlnotarealprefix");
  });
});

describe("applyPrefixChip", () => {
  it("prepends the chip's insert text to a plain query", () => {
    const symbolsChip = PREFIX_CHIPS.find((c) => c.hint === "symbols")!;
    expect(applyPrefixChip("Widget", symbolsChip)).toBe("@Widget");
  });

  it("replaces an existing prefix rather than stacking it", () => {
    const filesChip = PREFIX_CHIPS.find((c) => c.hint === "files")!;
    expect(applyPrefixChip("@Widget", filesChip)).toBe("#Widget");
  });

  it("is idempotent - clicking the same chip twice does not double the prefix", () => {
    const semanticChip = PREFIX_CHIPS.find((c) => c.hint === "semantic")!;
    const once = applyPrefixChip("how does auth work", semanticChip);
    const twice = applyPrefixChip(once, semanticChip);
    expect(once).toBe(twice);
    expect(once).toBe("?nl how does auth work");
  });
});
