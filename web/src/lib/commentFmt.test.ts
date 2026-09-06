import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import type { Anchor } from "../api/client";
import { relTime, anchorLabel, anchorLocationLabel } from "./commentFmt";

describe("anchorLabel", () => {
  it("labels a file anchor", () => {
    expect(anchorLabel({ anchor: { kind: "file" } })).toBe("file");
  });
  it("returns a short chapter path verbatim", () => {
    expect(anchorLabel({ anchor: { kind: "chapter", path: "Intro" } })).toBe("Intro");
  });
  it("keeps a chapter path of exactly 28 chars verbatim (boundary)", () => {
    const at = "b".repeat(28);
    expect(anchorLabel({ anchor: { kind: "chapter", path: at } })).toBe(at);
  });
  it("truncates a chapter path of 29 chars (first length past the > 28 cutoff)", () => {
    const over = "b".repeat(29);
    expect(anchorLabel({ anchor: { kind: "chapter", path: over } })).toBe("…" + "b".repeat(26));
  });
  it("truncates a long chapter path with a leading ellipsis (last 26 chars)", () => {
    const long = "a".repeat(40);
    const label = anchorLabel({ anchor: { kind: "chapter", path: long } });
    expect(label).toBe("…" + "a".repeat(26));
    expect(label.length).toBe(27);
  });
  it("prefixes a section id with § (first 24 chars)", () => {
    const a: Anchor = { kind: "section", id: "0123456789abcdef0123456789abcdef" };
    expect(anchorLabel({ anchor: a })).toBe("§ 0123456789abcdef01234567");
  });
  it("quotes and truncates a selection snippet (first 22 chars)", () => {
    const a: Anchor = {
      kind: "selection",
      css_path: "div>p",
      offset: 0,
      snippet: "the quick brown fox jumps over",
    };
    expect(anchorLabel({ anchor: a })).toBe("“the quick brown fox ju…”");
  });
});

describe("anchorLocationLabel", () => {
  // invariant:30 — anchorLabel's row badge text (pinned above) must not
  // regress when anchorLabel is refactored to compose this.
  it("matches anchorLabel for every non-section kind", () => {
    const cases: Anchor[] = [
      { kind: "file" },
      { kind: "chapter", path: "Intro" },
      { kind: "chapter", path: "a".repeat(40) },
      { kind: "selection", css_path: "div>p", offset: 0, snippet: "the quick brown fox jumps over" },
    ];
    for (const anchor of cases) {
      expect(anchorLocationLabel({ anchor })).toBe(anchorLabel({ anchor }));
    }
  });
  it("drops the section glyph anchorLabel adds (the citation template supplies its own)", () => {
    const a: Anchor = { kind: "section", id: "0123456789abcdef0123456789abcdef" };
    expect(anchorLocationLabel({ anchor: a })).toBe("0123456789abcdef01234567");
    expect(anchorLabel({ anchor: a })).toBe(`§ ${anchorLocationLabel({ anchor: a })}`);
  });
});

describe("relTime", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
  });
  afterEach(() => {
    vi.useRealTimers();
  });
  const ago = (sec: number) =>
    new Date(Date.parse("2026-01-01T00:00:00Z") - sec * 1000).toISOString();
  it("formats seconds", () => expect(relTime(ago(30))).toBe("30s ago"));
  it("formats minutes", () => expect(relTime(ago(5 * 60))).toBe("5m ago"));
  it("formats hours", () => expect(relTime(ago(2 * 3600))).toBe("2h ago"));
  it("formats days", () => expect(relTime(ago(3 * 86400))).toBe("3d ago"));
  it("clamps future timestamps to '0s ago'", () => expect(relTime(ago(-100))).toBe("0s ago"));
});
