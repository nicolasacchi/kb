import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import SearchResultCard from "./SearchResultCard";
import type { SearchHit } from "../../api/client";

const hit = (id: string, score: number | null): SearchHit =>
  ({
    id,
    title: `Doc ${id}`,
    path: `/x/${id}.html`,
    source_relative: `${id}.html`,
    kb: "alpha",
    kb_category: null,
    kb_status: null,
    kb_severity: null,
    tags: [],
    score,
  }) as unknown as SearchHit;

const bar = (h: SearchHit, scoreMax: number): string | null => {
  const html = renderToString(
    <MemoryRouter>
      <SearchResultCard hit={h} kb="alpha" query="x" scoreMax={scoreMax} showCorpus />
    </MemoryRouter>,
  );
  const m = html.match(/kb-search-card__score[^>]*><i style="width:(\d+)%/);
  return m ? m[1] : null;
};

describe("SearchResultCard score bar", () => {
  it("is score/scoreMax of the (fused) result set, floored at 4%", () => {
    // Two federated hits with different fused scores: bar widths are
    // proportional to score / max, and the weakest is clamped to >= 4%.
    expect(bar(hit("a", 0.0164), 0.0164)).toBe("100");
    expect(bar(hit("b", 0.0082), 0.0164)).toBe("50");
    expect(bar(hit("c", 0.0001), 0.0164)).toBe("4");
  });

  it("hides the bar when there is no score or no max", () => {
    expect(bar(hit("d", null), 0.0164)).toBeNull();
    expect(bar(hit("e", 0.01), 0)).toBeNull();
  });
});
