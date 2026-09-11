import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { FactsOut, LanesOut } from "../api/types";
import type { DiagnosticGutterMark } from "./diagnostics";
import golden from "./lanes.golden.json";
import {
  buildFactsView,
  coverageBandMarks,
  factsForLine,
  formatAgeSecs,
  formatFactValue,
  freshnessCaption,
  GIT_BEHAVIOR,
  groupLanes,
  isDiagnosticLane,
  isFamilyTemplate,
  laneDiagnosticMarks,
  mergeDiagnosticMarks,
  SECS_PER_DAY,
  trustClassName,
  trustClassOf,
} from "./lanes";

const registry = golden.registry as LanesOut;
const facts = golden.facts as FactsOut;

describe("freshnessCaption — display only, never mutates the wire class", () => {
  it("is null under retention/2, aging past that, stale past retention", () => {
    const retention = 14;
    expect(freshnessCaption(0, retention)).toBeNull();
    expect(freshnessCaption(retention * SECS_PER_DAY * 0.4, retention)).toBeNull();
    expect(freshnessCaption(retention * SECS_PER_DAY * 0.5 + 1, retention)).toBe("aging");
    expect(freshnessCaption(retention * SECS_PER_DAY + 1, retention)).toBe("stale");
  });

  it("does not rewrite fact.class on the golden rows", () => {
    const groups = groupLanes(registry, facts);
    const all = groups.flatMap((g) => g.facts);
    const wire = new Map((facts.facts ?? []).map((f) => [`${f.lane}:${f.kind}:${f.line ?? 0}:${f.reason}`, f.class]));
    for (const d of all) {
      const key = `${d.fact.lane}:${d.fact.kind}:${d.fact.line ?? 0}:${d.fact.reason}`;
      expect(d.fact.class, key).toBe(wire.get(key));
    }
  });

  it("captions the golden aging/stale rows from their own lane retention", () => {
    const groups = groupLanes(registry, facts);
    const cov = groups.find((g) => g.id === "coverage.simplecov")!;
    const aging = cov.facts.find((d) => d.fact.reason === "reanchored-exact");
    expect(aging?.freshness).toBe("aging");
    const stale = cov.facts.find((d) => d.fact.reason === "no-snippet");
    expect(stale?.freshness).toBe("stale");
    const rubocop = groups.find((g) => g.id === "rubocop")!;
    const staleCop = rubocop.facts.find((d) => d.fact.reason === "no-anchor");
    expect(staleCop?.freshness).toBe("stale");
  });
});

describe("groupLanes — the golden wire", () => {
  it("skips the sarif.* family template and keeps every addressable lane", () => {
    const groups = groupLanes(registry, facts);
    expect(groups.map((g) => g.id)).toEqual([
      "git.behavior",
      "coverage.simplecov",
      "rubocop",
      "sarif.brakeman",
      "coverage.off",
    ]);
    expect(groups.some((g) => g.id === "sarif.*")).toBe(false);
  });

  it("names disabled / empty / enabled with the wire's own reason", () => {
    const groups = groupLanes(registry, facts);
    const off = groups.find((g) => g.id === "coverage.off")!;
    expect(off.status).toBe("disabled");
    expect(off.statusReason).toContain("[lanes] enabled");
    const git = groups.find((g) => g.id === GIT_BEHAVIOR)!;
    expect(git.status).toBe("enabled");
    expect(git.facts).toHaveLength(3);
    const rubocop = groups.find((g) => g.id === "rubocop")!;
    expect(rubocop.status).toBe("enabled");
  });

  it("covers every trust class and every classing reason on the golden", () => {
    const classes = new Set((facts.facts ?? []).map((f) => f.class));
    expect([...classes].sort()).toEqual(["candidate", "exact", "likely", "orphan"]);
    const reasons = new Set((facts.facts ?? []).map((f) => f.reason));
    for (const r of [
      "blob-current",
      "blob-current-sha-attributed",
      "file-level-blob-moved",
      "reanchored-exact",
      "reanchored-fuzzy",
      "no-anchor",
      "path-gone",
      "no-snippet",
      "content-unreadable",
    ]) {
      expect(reasons.has(r), r).toBe(true);
    }
  });

  it("formats coverage / diagnostic / git.behavior values", () => {
    const rows = facts.facts ?? [];
    const hits = rows.find((f) => f.kind === "coverage" && f.reason === "blob-current")!;
    expect(formatFactValue(hits)).toBe("3 hits");
    const uncovered = rows.find((f) => f.kind === "coverage" && f.reason === "reanchored-exact")!;
    expect(formatFactValue(uncovered)).toBe("uncovered");
    const diag = rows.find((f) => f.lane === "rubocop" && f.reason === "blob-current")!;
    expect(formatFactValue(diag)).toContain("Style/X");
    expect(formatFactValue(diag)).toContain("Prefer y");
    const churn = rows.find((f) => f.kind === "churn")!;
    expect(formatFactValue(churn)).toContain("12 commits");
    const last = rows.find((f) => f.kind === "last_touch")!;
    expect(formatFactValue(last)).toContain("human");
    const co = rows.find((f) => f.kind === "co_change")!;
    expect(formatFactValue(co)).toContain("app/other.rb");
  });

  it("buildFactsView is ready on the golden and loading/error otherwise", () => {
    expect(
      buildFactsView({
        registry,
        facts,
        registryLoading: false,
        factsLoading: false,
        error: null,
      }).kind,
    ).toBe("ready");
    expect(
      buildFactsView({
        registry: undefined,
        facts: undefined,
        registryLoading: true,
        factsLoading: true,
        error: null,
      }).kind,
    ).toBe("loading");
    expect(
      buildFactsView({
        registry,
        facts,
        registryLoading: false,
        factsLoading: false,
        error: "daemon down",
      }).kind,
    ).toBe("error");
    expect(
      buildFactsView({
        registry,
        facts: { ...facts, truncated: true },
        registryLoading: false,
        factsLoading: false,
        error: null,
      }).kind,
    ).toBe("partial");
  });

  it("trust class names use line-style classes, never hue-only", () => {
    expect(trustClassName("exact")).toContain("kbc-trust-exact");
    expect(trustClassName("likely")).toContain("kbc-trust-likely");
    expect(trustClassName("candidate")).toContain("kbc-trust-candidate");
    expect(trustClassName("orphan")).toContain("kbc-fact-trust--orphan");
    expect(trustClassOf({ class: "mystery" } as never)).toBe("orphan");
  });
});

describe("gutter variants — still four slots", () => {
  it("lane diagnostics ride slot 3 with source=lane", () => {
    const marks = laneDiagnosticMarks(facts.facts ?? []);
    expect(marks.get(4)?.source).toBe("lane");
    expect(marks.get(4)?.severity).toBe("error");
    expect(marks.get(4)?.count).toBeGreaterThanOrEqual(2);
    expect(marks.get(9)?.source).toBe("lane");
    expect(isDiagnosticLane("rubocop")).toBe(true);
    expect(isDiagnosticLane("sarif.brakeman")).toBe(true);
    expect(isDiagnosticLane(GIT_BEHAVIOR)).toBe(false);
  });

  it("mergeDiagnosticMarks flags both without dropping either", () => {
    const lsp = new Map<number, DiagnosticGutterMark>([
      [4, { severity: "hint", title: "lsp", count: 1, source: "lsp" }],
    ]);
    const merged = mergeDiagnosticMarks(lsp, laneDiagnosticMarks(facts.facts ?? []));
    expect(merged.get(4)?.source).toBe("both");
    expect(merged.get(4)?.severity).toBe("error");
    expect(merged.get(9)?.source).toBe("lane");
  });

  it("coverage bands ride the blame gutter map; git.behavior is not in it", () => {
    const bands = coverageBandMarks(facts.facts ?? [], 10);
    expect(bands.get(1)).toBe("covered");
    expect(bands.get(2)).toBe("uncovered");
    expect(bands.get(5)).toBe("nodata");
    expect(bands.has(0)).toBe(false);
    for (const f of facts.facts ?? []) {
      if (f.lane === GIT_BEHAVIOR) expect(f.line ?? 0).toBe(0);
    }
  });

  it("factsForLine is inclusive of line_end", () => {
    const on4 = factsForLine(facts.facts ?? [], 4);
    expect(on4.some((f) => f.lane === "rubocop")).toBe(true);
    expect(on4.some((f) => f.lane === "sarif.brakeman")).toBe(true);
    expect(factsForLine(facts.facts ?? [], 5).some((f) => f.lane === "sarif.brakeman")).toBe(true);
    expect(factsForLine(facts.facts ?? [], 1).every((f) => f.kind === "coverage")).toBe(true);
  });

  it("CodeView still creates exactly four lineGutters — never a fifth", () => {
    const src = readFileSync(fileURLToPath(new URL("../components/CodeView.tsx", import.meta.url)), "utf8");
    const calls = [...src.matchAll(/createLineGutter\(/g)];
    expect(calls.length).toBe(4);
    expect(src).not.toContain("kbc-facts-gutter");
    expect(src).not.toContain("kbc-cov-gutter");
    expect(src).toContain("kbc-blame-gutter");
    expect(src).toContain("kbc-diag-gutter");
    expect(src).toContain("kbc-comment-gutter");
    expect(src).toContain("kbc-annot-gutter");
  });
});

describe("formatAgeSecs", () => {
  it("picks s/m/h/d", () => {
    expect(formatAgeSecs(9)).toBe("9s");
    expect(formatAgeSecs(120)).toBe("2m");
    expect(formatAgeSecs(7200)).toBe("2h");
    expect(formatAgeSecs(SECS_PER_DAY * 3)).toBe("3d");
  });
});

describe("family template", () => {
  it("isFamilyTemplate is true only for the sarif.* row", () => {
    const family = registry.lanes.find((l) => l.id === "sarif.*")!;
    expect(isFamilyTemplate(family)).toBe(true);
    expect(isFamilyTemplate(registry.lanes.find((l) => l.id === "sarif.brakeman")!)).toBe(false);
  });
});
