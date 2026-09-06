import { describe, expect, it } from "vitest";
import { formatInjectionBlock, formatInjectionLine } from "./injectionPreview";
import type { RecallHit } from "../api/client";

function hit(overrides: Partial<RecallHit> = {}): RecallHit {
  return {
    id: "aaaaaaaaaaaa",
    kb: "notes",
    title: "ingest retry cap",
    path: "/notes/ingest.html",
    source_relative: "ingest.html",
    score: 0.5,
    salience: 0.6,
    pinned: false,
    global: false,
    linked_kbs: [],
    recall_weekly: [],
    recall_count: 0,
    ...overrides,
  } as RecallHit;
}

describe("formatInjectionLine", () => {
  it("renders 'unread' when read_pct is absent", () => {
    const line = formatInjectionLine(hit());
    expect(line).toBe("- ingest retry cap  [notes]  (id aaaaaaaaaaaa, unread)");
  });

  it("renders read% and stopped-at when present", () => {
    const line = formatInjectionLine(hit({ read_pct: 42, stopped_at: "usage" }));
    expect(line).toBe(
      "- ingest retry cap  [notes]  (id aaaaaaaaaaaa, read 42% — stopped at usage)",
    );
  });

  it("renders read% without a stopped-at when the section is unknown", () => {
    const line = formatInjectionLine(hit({ read_pct: 10 }));
    expect(line).toBe("- ingest retry cap  [notes]  (id aaaaaaaaaaaa, read 10%)");
  });

  it("appends a ↳ summary continuation line when present", () => {
    const line = formatInjectionLine(hit({ summary: "a one-line gloss" }));
    expect(line).toBe(
      "- ingest retry cap  [notes]  (id aaaaaaaaaaaa, unread)\n    ↳ a one-line gloss",
    );
  });

  it("truncates the summary to 220 chars, matching the shell hook's jq slice", () => {
    const long = "x".repeat(500);
    const line = formatInjectionLine(hit({ summary: long }));
    const summaryLine = line.split("\n")[1];
    expect(summaryLine).toBe(`    ↳ ${"x".repeat(220)}`);
  });

  it("omits the continuation line entirely when summary is absent/empty", () => {
    expect(formatInjectionLine(hit({ summary: undefined }))).not.toContain("↳");
    expect(formatInjectionLine(hit({ summary: "" }))).not.toContain("↳");
  });
});

describe("formatInjectionBlock", () => {
  it("returns null for zero hits — nothing is injected, not an empty header", () => {
    expect(formatInjectionBlock([])).toBeNull();
  });

  it("renders the header + every hit line, in order", () => {
    const block = formatInjectionBlock([
      hit({ id: "aaaaaaaaaaaa", title: "Alpha" }),
      hit({ id: "bbbbbbbbbbbb", title: "Beta", read_pct: 90 }),
    ]);
    expect(block).toBe(
      "Relevant memories from kb (recall — these persist across sessions):\n" +
        "- Alpha  [notes]  (id aaaaaaaaaaaa, unread)\n" +
        "- Beta  [notes]  (id bbbbbbbbbbbb, read 90%)",
    );
  });
});
