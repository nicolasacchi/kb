import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { ViewBar } from "@kb/ui/ViewBar";
import {
  buildSearchClusters,
  isNoEmbeddingModelError,
  NO_MODEL_REASON,
  type SearchControlsArgs,
} from "./searchControls";

const base: SearchControlsArgs = {
  mode: "hybrid",
  scope: "one",
  limit: 50,
  sort: "relevance",
  noModel: false,
  onMode: () => {},
  onScope: () => {},
  onLimit: () => {},
  onSort: () => {},
};

describe("isNoEmbeddingModelError", () => {
  it("recognises the daemon's refusal and nothing else", () => {
    expect(isNoEmbeddingModelError("Bad Request: kb 'x' has no embedding_model configured")).toBe(true);
    expect(isNoEmbeddingModelError("network down")).toBe(false);
    expect(isNoEmbeddingModelError(null)).toBe(false);
  });
});

describe("buildSearchClusters", () => {
  it("lays out Mode, Scope, Results and Sort as four clusters", () => {
    expect(buildSearchClusters(base).map((c) => c.label)).toEqual(["Mode", "Scope", "Results", "Sort"]);
    const html = renderToString(<ViewBar clusters={buildSearchClusters(base)} />);
    expect(html).toContain("all corpora");
    expect(html).toContain("<option");
    expect(html).not.toContain("kb-viewbar__reason");
  });

  it("disables model-backed modes with a visible reason when the kb has no model", () => {
    const clusters = buildSearchClusters({ ...base, noModel: true });
    const mode = clusters[0].controls;
    expect(mode.find((c) => c.id === "mode-keyword")?.disabled).toBeUndefined();
    const model = mode.find((c) => c.id === "mode-model");
    expect(model?.disabled).toBe(true);
    expect(model?.reason).toBe(NO_MODEL_REASON);
    expect(renderToString(<ViewBar clusters={clusters} />)).toContain(NO_MODEL_REASON);
  });

  it("routes selections to the callbacks", () => {
    const seen: unknown[] = [];
    const c = buildSearchClusters({
      ...base,
      onMode: (m) => seen.push(m),
      onScope: (s) => seen.push(s),
      onLimit: (n) => seen.push(n),
      onSort: (s) => seen.push(s),
    });
    c[0].controls[0].onSelect?.("keyword");
    c[1].controls[0].onSelect?.("all");
    c[2].controls[0].onSelect?.("100");
    c[3].controls[0].onSelect?.("title");
    expect(seen).toEqual(["keyword", "all", 100, "title"]);
  });
});
