import { describe, expect, it } from "vitest";
import type { DiagnosticRow, DiagnosticsOut, RepoIntelStatus } from "../api/types";
import {
  buildDiagnosticsView,
  countBySeverity,
  detectLangId,
  diagnosticGutterMarks,
  diagnosticsChipText,
  effectiveIntelProviders,
  formatSeveritySummary,
  langCoveredByIntel,
  severityLabel,
  severityRank,
  sortDiagnosticsForCard,
  unavailableReasonLabel,
  worstSeverityLabel,
} from "./diagnostics";

function row(overrides: Partial<DiagnosticRow> = {}): DiagnosticRow {
  return {
    line: 1,
    col: 1,
    end_line: 0,
    end_col: 0,
    severity: 1,
    code: null,
    source: "ruby-lsp",
    message: "bad thing",
    ...overrides,
  };
}

function intel(overrides: Partial<RepoIntelStatus> = {}): RepoIntelStatus {
  return { provider: "ruby-lsp", langs: ["ruby"], alive: true, server_version: "1.0", ...overrides };
}

function out(overrides: Partial<DiagnosticsOut> = {}): DiagnosticsOut {
  return {
    schema: "diagnostics/1",
    path: "a.rb",
    diagnostics: [],
    provider: "ruby-lsp",
    fetched: true,
    unavailable_reason: null,
    ...overrides,
  };
}

describe("severityLabel / severityRank", () => {
  it("maps the raw LSP ints 1-4 to labels", () => {
    expect(severityLabel(1)).toBe("error");
    expect(severityLabel(2)).toBe("warning");
    expect(severityLabel(3)).toBe("info");
    expect(severityLabel(4)).toBe("hint");
  });

  it("degrades null/unrecognized to unknown", () => {
    expect(severityLabel(null)).toBe("unknown");
    expect(severityLabel(undefined)).toBe("unknown");
    expect(severityLabel(0)).toBe("unknown");
    expect(severityLabel(99)).toBe("unknown");
  });

  it("ranks error < warning < info < hint < unknown", () => {
    expect(severityRank(1)).toBeLessThan(severityRank(2));
    expect(severityRank(2)).toBeLessThan(severityRank(3));
    expect(severityRank(3)).toBeLessThan(severityRank(4));
    expect(severityRank(4)).toBeLessThan(severityRank(null));
  });
});

describe("countBySeverity", () => {
  it("buckets rows by severity label, tracking total", () => {
    const rows = [row({ severity: 1 }), row({ severity: 1 }), row({ severity: 2 }), row({ severity: null })];
    expect(countBySeverity(rows)).toEqual({ error: 2, warning: 1, info: 0, hint: 0, unknown: 1, total: 4 });
  });

  it("is empty for no rows", () => {
    expect(countBySeverity([])).toEqual({ error: 0, warning: 0, info: 0, hint: 0, unknown: 0, total: 0 });
  });
});

describe("worstSeverityLabel", () => {
  it("picks the most severe non-zero bucket", () => {
    expect(worstSeverityLabel(countBySeverity([row({ severity: 2 }), row({ severity: 1 })]))).toBe("error");
    expect(worstSeverityLabel(countBySeverity([row({ severity: 3 }), row({ severity: 2 })]))).toBe("warning");
  });

  it("falls back to unknown only when nothing named is present", () => {
    expect(worstSeverityLabel(countBySeverity([row({ severity: null })]))).toBe("unknown");
  });

  it("is null for an empty count", () => {
    expect(worstSeverityLabel(countBySeverity([]))).toBeNull();
  });
});

describe("formatSeveritySummary", () => {
  it("renders the design-addendum-2 example verbatim", () => {
    const rows = [
      row({ severity: 1 }),
      row({ severity: 1 }),
      row({ severity: 2 }),
      row({ severity: 2 }),
      row({ severity: 2 }),
      row({ severity: 2 }),
      row({ severity: 2 }),
    ];
    expect(formatSeveritySummary(countBySeverity(rows))).toBe("2 errors · 5 warnings");
  });

  it("singularizes a count of one", () => {
    expect(formatSeveritySummary(countBySeverity([row({ severity: 1 })]))).toBe("1 error");
  });

  it("skips zero buckets and empty is the empty string", () => {
    expect(formatSeveritySummary(countBySeverity([]))).toBe("");
  });

  it("folds unknown-only counts into a generic diagnostic label", () => {
    expect(formatSeveritySummary(countBySeverity([row({ severity: null })]))).toBe("1 diagnostic");
    expect(
      formatSeveritySummary(countBySeverity([row({ severity: null }), row({ severity: null })])),
    ).toBe("2 diagnostics");
  });
});

describe("sortDiagnosticsForCard", () => {
  it("sorts by severity rank, then line, then column", () => {
    const rows = [
      row({ severity: 2, line: 1, col: 1 }),
      row({ severity: 1, line: 5, col: 1 }),
      row({ severity: 1, line: 2, col: 9 }),
      row({ severity: 1, line: 2, col: 3 }),
    ];
    const sorted = sortDiagnosticsForCard(rows);
    expect(sorted.map((r) => [r.severity, r.line, r.col])).toEqual([
      [1, 2, 3],
      [1, 2, 9],
      [1, 5, 1],
      [2, 1, 1],
    ]);
  });

  it("does not mutate the input array", () => {
    const rows = [row({ line: 2 }), row({ line: 1 })];
    const copy = [...rows];
    sortDiagnosticsForCard(rows);
    expect(rows).toEqual(copy);
  });
});

describe("detectLangId", () => {
  it("mirrors kb-code-server's lang.rs extension table", () => {
    expect(detectLangId("src/main.rs")).toBe("rust");
    expect(detectLangId("app/models/user.rb")).toBe("ruby");
    expect(detectLangId("lib/thing.py")).toBe("python");
    expect(detectLangId("src/index.ts")).toBe("typescript");
    expect(detectLangId("src/App.tsx")).toBe("tsx");
    expect(detectLangId("src/index.js")).toBe("javascript");
    expect(detectLangId("bin/run.sh")).toBe("bash");
    expect(detectLangId("config.yml")).toBe("yaml");
    expect(detectLangId("config.yaml")).toBe("yaml");
    expect(detectLangId("main.go")).toBe("go");
    expect(detectLangId("Cargo.toml")).toBe("toml");
    expect(detectLangId("package.json")).toBe("json");
    expect(detectLangId("app/views/index.html.erb")).toBe("erb");
  });

  it("is null for no extension or an unknown extension", () => {
    expect(detectLangId("Makefile")).toBeNull();
    expect(detectLangId("README")).toBeNull();
    expect(detectLangId("image.png")).toBeNull();
  });

  it("is null for a dotfile with no real extension (dot is the basename)", () => {
    expect(detectLangId(".gitignore")).toBeNull();
  });
});

describe("langCoveredByIntel", () => {
  it("true only when intel is present and its langs cover the path's language", () => {
    expect(langCoveredByIntel("a.rb", intel({ langs: ["ruby"] }))).toBe(true);
    expect(langCoveredByIntel("a.py", intel({ langs: ["ruby"] }))).toBe(false);
  });

  it("false when intel is null/undefined (no provider configured for this repo)", () => {
    expect(langCoveredByIntel("a.rb", null)).toBe(false);
    expect(langCoveredByIntel("a.rb", undefined)).toBe(false);
  });

  it("false when path is null/undefined/empty (no file open)", () => {
    expect(langCoveredByIntel(null, intel())).toBe(false);
    expect(langCoveredByIntel(undefined, intel())).toBe(false);
    expect(langCoveredByIntel("", intel())).toBe(false);
  });

  it("does not consult intel.alive — a stale-looking snapshot still gates open", () => {
    expect(langCoveredByIntel("a.rb", intel({ alive: false }))).toBe(true);
  });
});

describe("diagnosticGutterMarks", () => {
  it("keys by line, worst severity wins on overlap", () => {
    const rows = [row({ line: 3, severity: 2, message: "warn here" }), row({ line: 3, severity: 1, message: "error here" })];
    const marks = diagnosticGutterMarks(rows);
    expect(marks.get(3)?.severity).toBe("error");
    expect(marks.get(3)?.count).toBe(2);
  });

  it("spans [line, end_line] when end_line >= line", () => {
    const rows = [row({ line: 3, end_line: 5, severity: 1 })];
    const marks = diagnosticGutterMarks(rows);
    expect([...marks.keys()].sort()).toEqual([3, 4, 5]);
  });

  it("treats end_line 0 (or less than line) as a single-line diagnostic", () => {
    const rows = [row({ line: 7, end_line: 0, severity: 1 })];
    const marks = diagnosticGutterMarks(rows);
    expect([...marks.keys()]).toEqual([7]);
  });

  it("titles a single-diagnostic line with its message, a multi-diagnostic line with a summary", () => {
    const single = diagnosticGutterMarks([row({ line: 1, message: "only one" })]);
    expect(single.get(1)?.title).toBe("only one");

    const multi = diagnosticGutterMarks([row({ line: 1, severity: 1 }), row({ line: 1, severity: 2 })]);
    expect(multi.get(1)?.title).toBe("1 error · 1 warning");
  });
});

describe("unavailableReasonLabel", () => {
  it("maps the closed vocabulary to human text", () => {
    expect(unavailableReasonLabel("unknown_language")).toBe("file type not recognized");
    expect(unavailableReasonLabel("no_provider_configured")).toBe(
      "no diagnostics provider configured for this file",
    );
    expect(unavailableReasonLabel("file_unreadable")).toBe("file could not be read");
    expect(unavailableReasonLabel("provider_unavailable")).toBe("diagnostics provider unavailable");
    expect(unavailableReasonLabel("blob_stale")).toBe("file changed while fetching diagnostics — try again");
  });

  it("degrades an unrecognized reason to itself verbatim (forward compat)", () => {
    expect(unavailableReasonLabel("some_future_reason")).toBe("some_future_reason");
  });

  it("has a generic fallback for a missing reason", () => {
    expect(unavailableReasonLabel(null)).toBe("diagnostics unavailable");
    expect(unavailableReasonLabel(undefined)).toBe("diagnostics unavailable");
  });
});

describe("buildDiagnosticsView — the state matrix", () => {
  it("absent when not covered by any provider, regardless of data/loading", () => {
    expect(buildDiagnosticsView(false, undefined, true)).toEqual({ kind: "absent" });
    expect(buildDiagnosticsView(false, out(), false)).toEqual({ kind: "absent" });
  });

  it("loading when covered but the query hasn't resolved yet", () => {
    expect(buildDiagnosticsView(true, undefined, true)).toEqual({ kind: "loading" });
    expect(buildDiagnosticsView(true, undefined, false)).toEqual({ kind: "loading" });
  });

  it("reason when diagnostics is null (no provider/refused)", () => {
    const view = buildDiagnosticsView(
      true,
      out({ diagnostics: null, unavailable_reason: "provider_unavailable" }),
      false,
    );
    expect(view).toEqual({ kind: "reason", reason: "diagnostics provider unavailable" });
  });

  it("clean when diagnostics is an empty array (provider ran, found nothing)", () => {
    expect(buildDiagnosticsView(true, out({ diagnostics: [] }), false)).toEqual({ kind: "clean" });
  });

  it("rows when diagnostics is non-empty, sorted + counted + worst computed", () => {
    const data = out({ diagnostics: [row({ severity: 2, line: 2 }), row({ severity: 1, line: 1 })] });
    const view = buildDiagnosticsView(true, data, false);
    expect(view.kind).toBe("rows");
    expect(view.rows?.map((r) => r.severity)).toEqual([1, 2]);
    expect(view.counts?.total).toBe(2);
    expect(view.worst).toBe("error");
  });
});

describe("diagnosticsChipText", () => {
  it("renders counts text only for the rows state", () => {
    const data = out({ diagnostics: [row({ severity: 1 }), row({ severity: 1 })] });
    const view = buildDiagnosticsView(true, data, false);
    expect(diagnosticsChipText(view)).toBe("2 errors");
  });

  it("is null for absent/loading/reason/clean (chip stays quiet; the card carries the full state)", () => {
    expect(diagnosticsChipText({ kind: "absent" })).toBeNull();
    expect(diagnosticsChipText({ kind: "loading" })).toBeNull();
    expect(diagnosticsChipText({ kind: "reason", reason: "x" })).toBeNull();
    expect(diagnosticsChipText({ kind: "clean" })).toBeNull();
  });
});

// ── S2-D (B4) — multi-provider intel status ────────────────────────────
describe("effectiveIntelProviders", () => {
  it("prefers a non-empty intel_providers vec", () => {
    const a = intel({ provider: "rust-analyzer" });
    const b = intel({ provider: "typescript-language-server" });
    expect(effectiveIntelProviders({ intel: a, intel_providers: [a, b] })).toEqual([a, b]);
  });

  it("falls back to the legacy single intel when intel_providers is absent or empty", () => {
    const a = intel();
    expect(effectiveIntelProviders({ intel: a })).toEqual([a]);
    expect(effectiveIntelProviders({ intel: a, intel_providers: [] })).toEqual([a]);
  });

  it("is empty when both are null/absent", () => {
    expect(effectiveIntelProviders({ intel: null })).toEqual([]);
    expect(effectiveIntelProviders({ intel: null, intel_providers: [] })).toEqual([]);
  });
});
