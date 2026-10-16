// @vitest-environment jsdom
//
// OL R2 — Passport: two lines (folder · category · mtime; id) and a Details
// disclosure holding file / created / indexed / metrics.
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import type { DocSummary } from "../../api/client";
import Passport from "./Passport";

afterEach(cleanup);

const doc = {
  id: "c765f4e41d6c0000",
  title: "T",
  source_relative: "notes/a-file.html",
  folder: "notes",
  kb_category: "design",
  mtime_unix: 1_790_000_000,
  created_unix: 1_780_000_000,
  indexed_at_unix: 1_790_000_100,
  word_count: 5000,
  svg_count: 2,
  table_count: 0,
  code_block_count: 1,
} as unknown as DocSummary;

describe("Passport", () => {
  it("shows two lines up front and keeps the rest under Details", () => {
    const { container } = render(
      <MemoryRouter>
        <Passport kb="k" doc={doc} />
      </MemoryRouter>,
    );
    const lines = container.querySelectorAll(".kb-pinsp__passport-line");
    expect(lines).toHaveLength(2);
    expect(lines[0].textContent).toContain("design");
    expect(lines[1].textContent).toContain("c765f4e41d6c");
    const details = container.querySelector("details.kb-pinsp__details")!;
    expect(details.hasAttribute("open")).toBe(false);
    const t = details.textContent!;
    for (const w of ["file", "created", "indexed", "words", "svgs", "code blocks"]) {
      expect(t).toContain(w);
    }
    expect(t).not.toContain("tables");
    // nothing from Details leaks into the two lines
    expect(lines[0].textContent).not.toContain("created");
  });
});
