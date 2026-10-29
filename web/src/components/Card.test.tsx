import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import ConfirmProvider from "./ConfirmProvider";
import Card from "./Card";
import type { DocSummary } from "../api/client";

function html(doc: Partial<DocSummary>, progress?: { pct: number; isDone: boolean }) {
  const d = {
    id: "abc123def456",
    title: "A Title",
    source_relative: "notes/a.html",
    folder: "notes",
    summary: "What it is about.",
    tags: ["one", "two", "three"],
    word_count: 1100,
    backlinks: 4,
    svg_count: 2,
    table_count: 1,
    pages: [{}, {}],
    ...doc,
  } as Record<string, unknown>;
  d.path = `/abs/${d.source_relative as string}`;
  const dd = d as unknown as DocSummary;
  return renderToString(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter>
        <ConfirmProvider>
          <Card doc={dd} kb="k" progress={progress} />
        </ConfirmProvider>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("Card (OL L4)", () => {
  it("shows one meta line with folder and N min read", () => {
    const h = html({});
    expect(h).toContain("kb-card__meta");
    expect(h).toContain("notes");
    expect(h).toContain("5 min read");
  });
  it("shows at most two tags", () => {
    const h = html({});
    expect(h).toContain("one");
    expect(h).toContain("two");
    expect(h).not.toContain(">three<");
  });
  it("drops glyphs, word tape, counts and MD/pages badges from the face", () => {
    const h = html({ source_relative: "notes/a.md" });
    for (const c of [
      "kb-card__tape",
      "kb-card__words",
      "kb-card__links",
      "kb-card__coderefs",
      "kb-card__residue",
      "kb-card__badge--pages",
      "kb-card__badge--markdown",
      "prev-hyb-glyph",
    ]) {
      expect(h).not.toContain(c);
    }
  });
  it("keeps aria-labels, the three actions and the index badge", () => {
    const h = html({ title: "Home", source_relative: "index.html" });
    expect(h).toContain('aria-label="Open Home"');
    expect(h).toContain('aria-label="Preview Home"');
    expect(h).toContain('aria-label="Download Home"');
    expect(h).toContain('aria-label="Exclude Home from the index"');
    expect(h).toContain("⌂ index");
  });
  it("renders one closed More-actions button and no sheet markup", () => {
    const h = html({ title: "Home", source_relative: "index.html" });
    expect(h).toContain('data-kb-act="more"');
    expect(h).toContain('aria-label="More actions for Home"');
    expect(h).toContain('aria-haspopup="dialog"');
    expect(h).toContain('aria-expanded="false"');
    expect(h).not.toContain("data-kb-sheet");
    expect(h).not.toContain('role="dialog"');
  });
  it("renders reading progress as a bottom bar, only when there is any", () => {
    expect(html({}, { pct: 40, isDone: false })).toContain('aria-label="40% read"');
    expect(html({}, { pct: 40, isDone: false })).toContain("width:40%");
    expect(html({}, { pct: 100, isDone: true })).toContain("kb-card__progress--done");
    expect(html({})).not.toContain("kb-card__progress");
  });
});
