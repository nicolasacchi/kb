import type { ReactNode } from "react";
import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import VirtualList from "./VirtualList";
import ListRow from "./ListRow";
import type { DocSummary } from "../api/client";

const doc = {
  id: "abc123def456",
  title: "A Title",
  path: "/abs/notes/a.html",
  source_relative: "notes/a.html",
  folder: "notes",
  kb_category: "research",
  word_count: 12345,
  backlinks: 3,
  session_residue: 2,
  svg_count: 2,
  read_pct: 42,
  mtime_unix: 1,
} as unknown as DocSummary;

function wrap(node: ReactNode, url = "/?view=list") {
  return renderToString(
    <QueryClientProvider client={new QueryClient()}>
      <MemoryRouter initialEntries={[url]}>{node}</MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("Index view (OL L5)", () => {
  it("renders a sticky header with the default columns and a Columns button", () => {
    const h = wrap(<VirtualList docs={[doc]} kb="k" />);
    expect(h).toContain('data-testid="index-head"');
    expect(h).toContain('data-col="title"');
    expect(h).toContain('data-col="words"');
    expect(h).toContain('data-col="age"');
    expect(h).not.toContain('data-col="category"');
    expect(h).toContain('data-testid="index-columns"');
  });

  it("marks the active sort header with aria-sort from the URL", () => {
    const h = wrap(<VirtualList docs={[doc]} kb="k" />, "/?view=list&sort=words&dir=asc");
    expect(h).toMatch(/data-col="words"[^>]*aria-sort="ascending"|aria-sort="ascending"[^>]*data-col="words"/);
  });

  it("a row renders only the visible columns", () => {
    const base = wrap(<ListRow doc={doc} kb="k" />);
    expect(base).toContain("12,345");
    expect(base).not.toContain("list-row__cat");
    const all = wrap(
      <ListRow
        doc={doc}
        kb="k"
        refs={5}
        columns={["title", "folder", "category", "words", "links", "refs", "kept", "caps", "read", "age"]}
      />,
    );
    expect(all).toContain("research");
    expect(all).toContain("list-row__links");
    expect(all).toContain("42%");
    expect(all).toContain("prev-hyb-glyphs");
    expect(all).toMatch(/list-row__refs[^>]*>5</);
    expect(all).toMatch(/list-row__kept[^>]*>2</);
  });
});
