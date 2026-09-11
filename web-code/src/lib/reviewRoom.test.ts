// V76-R2a — the Room's chip/icon mappings (golden), hero derivations, and
// truncation helpers. The golden pins every (severity, act, category) →
// (token, icon) pair so a later edit that silently recolours an axis fails
// HERE, by name.
import { describe, expect, it } from "vitest";
import type { ReviewFileRow, ReviewFinding } from "../api/types";
import { Icon } from "../components/icons";
import {
  ACT_CHIPS,
  CATEGORY_CHIPS,
  CATEGORY_FALLBACK_TOKENS,
  SECTION_DECORS,
  SEVERITY_CHIPS,
  actChip,
  categoryChip,
  filesViewedOf,
  heroAgentOf,
  heroBaseSourceOf,
  heroCounts,
  heroLedeOf,
  nextRoomDensity,
  parseRoomDensity,
  sectionDecor,
  severityChip,
  truncateMiddle,
} from "./reviewRoom";

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-x",
    severity: "concern",
    category: "Correctness",
    location: { kind: "whole_file", path: "a.rb", lines: null, removed: false },
    title: "t",
    rationale: "r",
    recommendation: null,
    evidence: null,
    origin: "import",
    author: "claude",
    disposition: null,
    published_state: "unpublished",
    published_at: null,
    published_url: null,
    superseded: false,
    superseded_reason: null,
    content_updated_at: null,
    annotation_id: "a1",
    import_batch_id: "b1",
    created_at: 1,
    updated_at: 1,
    resolution: { line: null, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

function fileRow(overrides: Partial<ReviewFileRow> = {}): ReviewFileRow {
  return {
    path: "a.rb",
    old_path: null,
    status: "modified",
    additions: 1,
    deletions: 1,
    blob_sha: "b",
    viewed: false,
    viewed_stale: false,
    open_annotations: 0,
    ...overrides,
  };
}

describe("the chip golden — severity × act × category → token + icon", () => {
  it("severity chips (the server's closed 3-value vocabulary)", () => {
    expect(severityChip("blocker")).toEqual({ token: "--red", icon: "Warn" });
    expect(severityChip("concern")).toEqual({ token: "--warn", icon: "Warn" });
    expect(severityChip("ok")).toEqual({ token: "--green", icon: "Check" });
    expect(Object.keys(SEVERITY_CHIPS).sort()).toEqual(["blocker", "concern", "ok"]);
  });

  it("act chips — the eight known acts plus the neutral fallback", () => {
    expect(actChip(undefined)).toEqual(ACT_CHIPS.issue); // absent act IS issue
    expect(actChip("issue")).toEqual({ token: "--red", icon: "Warn" });
    expect(actChip("question")).toEqual({ token: "--blue", icon: "Comment" });
    expect(actChip("suggestion")).toEqual({ token: "--accent-soft", icon: "Pen" });
    expect(actChip("nitpick")).toEqual({ token: "--ink-dim", icon: "Dot" });
    expect(actChip("praise")).toEqual({ token: "--green", icon: "Spark" });
    expect(actChip("note")).toEqual({ token: "--ink-mute", icon: "Note" });
    expect(actChip("todo")).toEqual({ token: "--warn", icon: "Tasks" });
    expect(actChip("chore")).toEqual({ token: "--ink-mute", icon: "Refresh" });
    // A future daemon's new act degrades NEUTRALLY — never another act's
    // colour, never a crash. Case-insensitive on known acts.
    expect(actChip("blockerino")).toEqual({ token: "--ink-mute", icon: "Dot" });
    expect(actChip("Praise")).toEqual(ACT_CHIPS.praise);
  });

  it("category chips — known categories pinned, unknowns deterministic", () => {
    expect(categoryChip("Correctness")).toEqual({ token: "--red", icon: "Warn" });
    expect(categoryChip("security")).toEqual({ token: "--red", icon: "Unlink" });
    expect(categoryChip("Performance")).toEqual({ token: "--warn", icon: "Flame" });
    expect(categoryChip("style")).toEqual({ token: "--blue", icon: "Palette" });
    expect(categoryChip("docs")).toEqual({ token: "--green", icon: "Note" });
    expect(categoryChip("tests")).toEqual({ token: "--blue", icon: "Check" });
    expect(categoryChip("maintainability")).toEqual({ token: "--accent-soft", icon: "Layers" });
    // Unknown category: the SAME answer on every call, from the safe list,
    // never a verdict red.
    const a = categoryChip("Observability");
    const b = categoryChip("Observability");
    expect(a).toEqual(b);
    expect(a.icon).toBe("Bookmark");
    expect(CATEGORY_FALLBACK_TOKENS).toContain(a.token);
    expect(a.token).not.toBe("--red");
  });

  it("section decorators — one icon + one token per kind", () => {
    expect(sectionDecor("summary")).toEqual({ token: "--accent-soft", icon: "Note", label: "Summary" });
    expect(sectionDecor("findings")).toEqual({ token: "--warn", icon: "Warn", label: "Findings" });
    expect(sectionDecor("praise")).toEqual({ token: "--green", icon: "Spark", label: "Praise" });
    expect(sectionDecor("questions")).toEqual({ token: "--blue", icon: "Comment", label: "Questions" });
    expect(sectionDecor("verdict")).toEqual({ token: "--accent", icon: "ClipboardCheck", label: "Verdict" });
    expect(sectionDecor("timeline")).toEqual({ token: "--ink-mute", icon: "Clock", label: "Timeline" });
    expect(Object.keys(SECTION_DECORS).sort()).toEqual(
      ["findings", "praise", "questions", "summary", "timeline", "verdict"].sort(),
    );
  });

  it("every mapped icon name is a REAL key of the icon set", () => {
    const names = new Set<string>();
    for (const spec of Object.values(SEVERITY_CHIPS)) names.add(spec.icon);
    for (const spec of Object.values(ACT_CHIPS)) names.add(spec.icon);
    for (const spec of Object.values(CATEGORY_CHIPS)) names.add(spec.icon);
    for (const spec of Object.values(SECTION_DECORS)) names.add(spec.icon);
    names.add("Dot"); // both fallbacks
    names.add("Bookmark"); // the category fallback
    for (const n of names) {
      expect(Icon[n as keyof typeof Icon], `Icon.${n} does not exist`).toBeTruthy();
    }
  });
});

describe("hero derivations — every number off the wire", () => {
  it("heroCounts IS ReportPanel's liveFindingCounts (superseded excluded)", () => {
    const counts = heroCounts([
      finding({ severity: "blocker" }),
      finding({ severity: "concern" }),
      finding({ severity: "ok" }),
      finding({ severity: "blocker", superseded: true }),
    ]);
    expect(counts).toEqual({ blockers: 1, concerns: 1, verified: 1 });
  });

  it("filesViewedOf counts viewed && !viewed_stale (K2a's viewed state)", () => {
    const v = filesViewedOf([
      fileRow({ viewed: true }),
      fileRow({ viewed: true, viewed_stale: true }),
      fileRow({}),
    ]);
    expect(v).toEqual({ viewed: 1, total: 3 });
  });

  it("heroAgentOf is null when NOTHING names an agent — never an UNSET box", () => {
    expect(heroAgentOf({}, null)).toBeNull();
    expect(heroAgentOf({ authored_by: "  " }, null)).toBeNull();
    expect(heroAgentOf({ authored_by: "claude-opus" }, null)).toEqual({ author: "claude-opus", sessionId: undefined });
    expect(heroAgentOf({}, "sess-1")).toEqual({ author: undefined, sessionId: "sess-1" });
    // the report's own session_id wins over the review's binding
    expect(heroAgentOf({ authored_by: "claude", session_id: "s-report" }, "s-review")).toEqual({
      author: "claude",
      sessionId: "s-report",
    });
  });

  it("heroLedeOf prefers the deck, else the summary's first paragraph", () => {
    expect(heroLedeOf({ deck: "the deck", summary: "s1\n\ns2" })).toBe("the deck");
    expect(heroLedeOf({ summary: "first para\n\nsecond para" })).toBe("first para");
    expect(heroLedeOf({})).toBeNull();
  });

  it("heroBaseSourceOf renders only a real string (absent on today's wire)", () => {
    expect(heroBaseSourceOf({})).toBeNull();
    expect(heroBaseSourceOf({ base_source: 42 })).toBeNull();
    expect(heroBaseSourceOf({ base_source: "stack" })).toBe("stack");
  });
});

describe("truncateMiddle", () => {
  it("returns short values untouched", () => {
    expect(truncateMiddle("src/a.ts", 48)).toBe("src/a.ts");
    expect(truncateMiddle("x".repeat(48), 48)).toBe("x".repeat(48));
  });

  it("keeps both ends of a long path, joined by one ellipsis", () => {
    const long = "apps/web/src/components/very/deeply/nested/module/file.tsx";
    const out = truncateMiddle(long, 32);
    expect(out.length).toBe(32);
    expect(out.startsWith("apps/web/src/com")).toBe(true);
    expect(out.endsWith("module/file.tsx")).toBe(true);
    expect(out).toContain("…");
  });

  it("falls back to a head slice under a degenerate max", () => {
    expect(truncateMiddle("abcdefghij", 5)).toBe("abcde");
  });
});

describe("the Room density pair", () => {
  it("parses totally and toggles", () => {
    expect(parseRoomDensity(null)).toBe("comfortable");
    expect(parseRoomDensity("compact")).toBe("compact");
    expect(parseRoomDensity("junk")).toBe("comfortable");
    expect(nextRoomDensity("compact")).toBe("comfortable");
    expect(nextRoomDensity("comfortable")).toBe("compact");
  });
});
