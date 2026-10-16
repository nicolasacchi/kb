import { describe, it, expect } from "vitest";
import {
  kbDocsCaption,
  kbDocsRowCount,
  kbDocsRowHref,
  safeDocUrl,
  shouldQueryKbDocs,
} from "./kbDocs";
import type { KbSearchOut } from "../api/types";

const hit = (url: string) => ({ kb: "platform", id: "a", title: "T", url, snippet: null });
const ok = (...urls: string[]): KbSearchOut => ({
  hits: urls.map(hit),
  kb_public_url: "https://kb.example.com",
});

describe("shouldQueryKbDocs", () => {
  it("needs two non-blank characters", () => {
    expect(shouldQueryKbDocs("")).toBe(false);
    expect(shouldQueryKbDocs("a")).toBe(false);
    expect(shouldQueryKbDocs("  a ")).toBe(false);
    expect(shouldQueryKbDocs("ab")).toBe(true);
    expect(shouldQueryKbDocs("theme family")).toBe(true);
  });

  it("stays out of the scoped code prefixes and command mode", () => {
    for (const q of ["@resolve", "#main", "/needle", "~session", ">theme"]) {
      expect(shouldQueryKbDocs(q), q).toBe(false);
    }
    // ...but not when the prefix char is merely inside the query.
    expect(shouldQueryKbDocs("the @ sign")).toBe(true);
  });
});

describe("kbDocsRowCount / kbDocsRowHref", () => {
  it("counts hits and resolves a row to its permalink", () => {
    const res = ok("https://kb.example.com/a/p/x.html", "https://kb.example.com/a/p/y.html");
    expect(kbDocsRowCount(res)).toBe(2);
    expect(kbDocsRowHref(res, 1)).toBe("https://kb.example.com/a/p/y.html");
    expect(kbDocsRowHref(res, 2)).toBeNull();
    expect(kbDocsRowHref(res, -1)).toBeNull();
  });

  it("an unavailable (or absent) response has no navigable rows", () => {
    const down: KbSearchOut = { hits: [], kb_public_url: "", unavailable: "kb unreachable" };
    expect(kbDocsRowCount(down)).toBe(0);
    expect(kbDocsRowHref(down, 0)).toBeNull();
    expect(kbDocsRowCount(null)).toBe(0);
    expect(kbDocsRowHref(null, 0)).toBeNull();
  });

  it("never opens a non-http(s) url", () => {
    expect(kbDocsRowHref(ok("javascript:alert(1)"), 0)).toBeNull();
    expect(kbDocsRowHref(ok("data:text/html,x"), 0)).toBeNull();
    expect(safeDocUrl("not a url")).toBeNull();
    expect(safeDocUrl("http://127.0.0.1:4000/a/p/x.html")).toBe("http://127.0.0.1:4000/a/p/x.html");
  });
});

describe("kbDocsCaption", () => {
  it("words the three relay captions and passes an unknown one through", () => {
    expect(kbDocsCaption("kb disabled")).toMatch(/not configured/);
    expect(kbDocsCaption("kb unreachable")).toMatch(/unreachable/);
    expect(kbDocsCaption("kb_sibling_mismatch")).toMatch(/sibling contract/);
    expect(kbDocsCaption("something new")).toBe("something new");
  });
});
