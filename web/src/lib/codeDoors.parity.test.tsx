// @vitest-environment jsdom
//
// OL IDEA6 - the door never disagrees with the Links tab. CodeRefsSection (the
// Links tab's Code section, rendered as-is) and codeDoorFor are fed the SAME
// lens; for every ref whose row renders exactly one link, the door's href is
// that link's href. The places the two deliberately differ are asserted as
// such, not left implicit.
import { describe, expect, it, vi } from "vitest";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import type { UseQueryResult } from "@tanstack/react-query";
import CodeRefsSection from "../components/CodeRefsSection";
import type { DocLensOut, DocLensScorecardOut } from "../api/doclens";
import { codeDoorFor } from "./codeDoors";
import { ref, row } from "./codeDoors.fixtures";

const CODE = "https://code.example";

const refs = [
  ref({ ordinal: 0, kind: "path_line", raw: "crates/kb-core/src/present.rs:120", path_hint: "crates/kb-core/src/present.rs", line_start: 120 }),
  ref({ ordinal: 1, kind: "path_line", raw: "crates/kb-core/src/drifted.rs:50", path_hint: "crates/kb-core/src/drifted.rs", line_start: 50 }),
  ref({ ordinal: 2, kind: "path", raw: "crates/kb-core/src/absent.rs", path_hint: "crates/kb-core/src/absent.rs" }),
  ref({ ordinal: 3, kind: "path", raw: "crates/kb-core/src/many.rs", path_hint: "crates/kb-core/src/many.rs" }),
  ref({ ordinal: 4, kind: "symbol_method", raw: "Algolia::Search#unique", symbol_container: "Algolia::Search", symbol_member: "unique" }),
  // recorded divergences
  ref({ ordinal: 5, kind: "path", raw: "crates/kb-core/src/few.rs", path_hint: "crates/kb-core/src/few.rs" }),
  ref({ ordinal: 6, kind: "symbol_method", raw: "Algolia::Search#either", symbol_container: "Algolia::Search", symbol_member: "either" }),
  ref({ ordinal: 7, kind: "issue", raw: "https://github.com/acme/widgets/issues/9", path_hint: "acme/widgets", line_start: 9 }),
  ref({ ordinal: 8, kind: "external", raw: "vendor/gems/gem.rb", path_hint: "vendor/gems/gem.rb" }),
];

const lensRefs = [
  row({ ordinal: 0, kind: "path_line", raw: refs[0].raw, path_state: "present", line_state: "confirmed", reader: { repo: "app", path: "crates/kb-core/src/present.rs", line: 120 } }),
  row({ ordinal: 1, kind: "path_line", raw: refs[1].raw, path_state: "present", line_state: "drifted", resolved_line: 61, reader: { repo: "app", path: "crates/kb-core/src/drifted.rs", line: 61 } }),
  row({ ordinal: 2, kind: "path", raw: refs[2].raw, path_state: "absent", search: { q: "absent.rs", repo: "app" } }),
  row({ ordinal: 3, kind: "path", raw: refs[3].raw, path_state: "ambiguous", candidate_count: 9, search: { q: "many.rs", repo: "app" } }),
  row({
    ordinal: 4, kind: "symbol_method", raw: refs[4].raw, symbol_state: "hit_unique", symbol_hit_count: 1,
    symbol_hits: [{ path: "app/services/search.rb", line_start: 14, line_end: 20, kind: "method", container: null }],
  }),
  row({ ordinal: 5, kind: "path", raw: refs[5].raw, path_state: "ambiguous", candidate_count: 2, candidates: ["a/few.rs", "b/few.rs"], search: { q: "few.rs", repo: "app" } }),
  row({
    ordinal: 6, kind: "symbol_method", raw: refs[6].raw, symbol_state: "hit_ambiguous", symbol_hit_count: 2,
    symbol_hits: [
      { path: "app/a.rb", line_start: 1, line_end: 2, kind: "method", container: null },
      { path: "app/b.rb", line_start: 3, line_end: 4, kind: "method", container: null },
    ],
  }),
  row({ ordinal: 7, kind: "issue", raw: refs[7].raw, issue: { owner: "acme", repo: "widgets", number: 9, href: "https://github.com/acme/widgets/issues/9" } }),
  row({ ordinal: 8, kind: "external", raw: refs[8].raw, path_state: "external" }),
];

const lens = {
  schema: "codelens/1",
  repo: { name: "app", head_sha: "abcdef1234", dirty: false },
  resolved_unix: 1_700_000_000,
  truncated: false,
  partial: false,
  partial_reason: null,
  counts: { total: 9, resolved: 9, present: 2, ambiguous: 2, absent: 1, external: 1, confirmed: 1, drifted: 1, unverifiable: 0, line_absent: 0, declared_but_absent: 0 },
  ungrouped_count: 9,
  groups: [],
  refs: lensRefs,
} as unknown as DocLensOut;

const scorecard = {
  repos: [{ name: "app", state: "ready", present: 2, ambiguous: 2, absent: 1, external: 1, dirty: false, reason: null }],
  pinned_repo: "app",
} as unknown as DocLensScorecardOut;

function q<T>(data: T): UseQueryResult<T> {
  return { data, isError: false, error: null, refetch: vi.fn() } as unknown as UseQueryResult<T>;
}

function renderRows(): HTMLElement {
  const html = renderToStaticMarkup(
    <MemoryRouter>
      <CodeRefsSection
        kb="canon"
        docId="abc"
        docPath="doc.html"
        refCount={9}
        extractedAt={1_700_000_000}
        codeRefsTruncated={false}
        neverScanned={false}
        codeUrl={CODE}
        repoParam="app"
        onPickRepo={() => {}}
        scorecardQuery={q(scorecard)}
        docLensQuery={q(lens)}
      />
    </MemoryRouter>,
  );
  const host = document.createElement("div");
  host.innerHTML = html;
  return host;
}

function linksFor(host: HTMLElement, raw: string): string[] {
  const rows = Array.from(host.querySelectorAll(".kb-pinsp__coderef-row")).filter((r) =>
    (r.textContent ?? "").includes(raw),
  );
  expect(rows, `one row for ${raw}`).toHaveLength(1);
  return Array.from(rows[0].querySelectorAll("a[href]")).map((a) => a.getAttribute("href") ?? "");
}

const ctx = { codeUrl: CODE, repo: "app", lens } as const;

describe("a door equals the Links tab's link", () => {
  const host = renderRows();
  for (const i of [0, 1, 2, 3, 4]) {
    it(`ref ${i} (${refs[i].raw})`, () => {
      const links = linksFor(host, refs[i].raw);
      expect(links).toHaveLength(1);
      const door = codeDoorFor(refs[i], ctx);
      expect(door, "has a door").not.toBeNull();
      expect(door!.href).toBe(links[0]);
    });
  }
});

describe("recorded divergences", () => {
  const host = renderRows();
  it("ambiguous <= 3: the row lists candidates, the door goes to the search", () => {
    const links = linksFor(host, refs[5].raw);
    expect(links.length).toBeGreaterThan(1);
    const door = codeDoorFor(refs[5], ctx)!;
    expect(door.href).toBe("https://code.example/search?q=few.rs&repo=app");
    expect(links).not.toContain(door.href);
  });
  it("ambiguous symbol: the row lists the hits, there is no door", () => {
    expect(linksFor(host, refs[6].raw).length).toBeGreaterThan(1);
    expect(codeDoorFor(refs[6], ctx)).toBeNull();
  });
  it("issue: the row links to GitHub, there is no door (it is already an <a>)", () => {
    expect(linksFor(host, refs[7].raw)).toEqual(["https://github.com/acme/widgets/issues/9"]);
    expect(codeDoorFor(refs[7], ctx)).toBeNull();
  });
  it("external: neither a link nor a door", () => {
    expect(linksFor(host, refs[8].raw)).toEqual([]);
    expect(codeDoorFor(refs[8], ctx)).toBeNull();
  });
});
