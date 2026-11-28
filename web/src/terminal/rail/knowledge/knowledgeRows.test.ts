import { describe, expect, it } from "vitest";
import { REF_MAX, RESEARCH_CAP, flattenRef, fromRecall, groupResearch, rankLabel, rowTitle } from "./knowledgeRows";

describe("flattenRef", () => {
  it("collapses newline, CR and tab runs to one space and trims", () => {
    const out = flattenRef({ title: "  a\n\nb\r\nc\td ", kb: "mem", source_relative: "x/y.html" });
    expect(out).toBe("[a b c d] (mem/x/y.html)");
    expect(out).not.toMatch(/[\n\r\t]/);
  });
  it("caps at 400 chars and never ends with a newline", () => {
    const out = flattenRef({ title: "t".repeat(900), kb: "mem", source_relative: "x.html" });
    expect(out.length).toBeLessThanOrEqual(REF_MAX);
    expect(out.endsWith("\n")).toBe(false);
  });
  it("keeps the (kb/path) locator when a long title is cut", () => {
    const out = flattenRef({ title: "t".repeat(900), kb: "mem", source_relative: "x/y.html" });
    expect(out.length).toBeLessThanOrEqual(REF_MAX);
    expect(out.endsWith("] (mem/x/y.html)")).toBe(true);
  });
  it("strips C0 and DEL bytes", () => {
    const out = flattenRef({ title: "a\u001b[31mb\u0007\u007fc", kb: "m" });
    expect(out).toBe("[a[31mbc] (m)");
    expect(out).not.toMatch(/[\u0000-\u001f\u007f]/);
  });
  it("names the kb alone when there is no source path", () => {
    expect(flattenRef({ title: "t", kb: "mem" })).toBe("[t] (mem)");
  });
  it("uses the unresolved placeholder for a missing title", () => {
    expect(rowTitle({ title: undefined })).toBe("(memory no longer resolves)");
  });
});

describe("rankLabel", () => {
  it("shows #n only when the capture recorded pos", () => {
    expect(rankLabel(3)).toBe("#3");
    expect(rankLabel(undefined)).toBe("rank unknown");
  });
  it("is never derived from list order", () => {
    const rows = [
      fromRecall({ kb: "m", id: "a", used: false }),
      fromRecall({ kb: "m", id: "b", used: true, pos: 7 }),
    ];
    expect(rows.map((r) => rankLabel(r.pos))).toEqual(["rank unknown", "#7"]);
  });
});

describe("groupResearch", () => {
  it("groups by kind in first-seen order and caps at 50", () => {
    const rows = Array.from({ length: 60 }, (_, i) => ({ kind: i % 2 ? "web" : "kb_search", query: `q${i}` }));
    const g = groupResearch(rows);
    expect(g.shown).toBe(RESEARCH_CAP);
    expect(g.total).toBe(60);
    expect(g.groups.map((x) => x.label)).toEqual(["kb", "web"]);
  });
});
