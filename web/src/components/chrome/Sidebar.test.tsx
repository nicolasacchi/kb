// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";

vi.mock("../../api/client", () => ({
  isAbortError: () => false,
  fetchKbs: async () => [{ name: "canon", doc_count: 3, memory_scope: false }],
  fetchTags: async () => [{ name: "pm", count: 5, color_seed: 10 }],
  fetchFolders: async () => ({ folders: [] }),
  fetchFacets: async () => ({
    categories: [
      { value: "note", count: 5 },
      { value: "report", count: 4 },
    ],
  }),
}));

import Sidebar from "./Sidebar";

afterEach(cleanup);

describe("Sidebar facet rows", () => {
  it("category rows have the same dot / name / count structure as tag rows, with the full value as the name", async () => {
    render(
      <MemoryRouter initialEntries={["/?kb=canon"]}>
        <Sidebar />
      </MemoryRouter>,
    );
    await waitFor(() => {
      expect(document.querySelectorAll(".kb-side__row").length).toBe(3);
    });
    const rows = Array.from(document.querySelectorAll(".kb-side__row"));
    const shape = (r: Element) =>
      Array.from(r.children).map((c) => c.className.split(" ")[0]);
    const tagRow = rows[0];
    const catRows = rows.slice(1);
    expect(shape(tagRow)).toEqual(["kb-side__dot", "kb-side__name", "kb-side__ct"]);
    for (const r of catRows) expect(shape(r)).toEqual(shape(tagRow));
    expect(catRows.map((r) => r.querySelector(".kb-side__name")?.textContent)).toEqual([
      "note",
      "report",
    ]);
  });
});
