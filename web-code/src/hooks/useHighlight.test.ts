import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import type { HighlightBatchOut } from "../api/types";
import {
  chunkHighlightBatch,
  highlightCacheKey,
  HIGHLIGHT_BATCH_MAX_BYTES,
  HIGHLIGHT_BATCH_MAX_ITEMS,
  HIGHLIGHT_SNIPPET_MAX_BYTES,
  skippedHighlightIds,
  uniqueHighlightItems,
  useHighlight,
  type HighlightItem,
} from "./useHighlight";

// 3-byte chars: over the byte cap while UNDER it as a UTF-16 length.
const OVERSIZE = "漢".repeat(Math.floor(HIGHLIGHT_SNIPPET_MAX_BYTES / 3) + 1);

describe("highlightCacheKey", () => {
  it("is stable for the same (lang, text)", () => {
    expect(highlightCacheKey("ruby", "def x\nend\n")).toBe(highlightCacheKey("ruby", "def x\nend\n"));
  });

  it("differs when lang or text differs", () => {
    const a = highlightCacheKey("ruby", "a");
    const b = highlightCacheKey("rust", "a");
    const c = highlightCacheKey("ruby", "b");
    expect(a).not.toBe(b);
    expect(a).not.toBe(c);
  });

  it("path only participates when lang is null", () => {
    expect(highlightCacheKey("ruby", "a", "x.rb")).toBe(highlightCacheKey("ruby", "a", "y.rs"));
    expect(highlightCacheKey(null, "a", "x.rb")).not.toBe(highlightCacheKey(null, "a", "y.rs"));
  });
});

describe("uniqueHighlightItems — batching", () => {
  it("collapses duplicate (lang,text) into one batch payload", () => {
    const items: HighlightItem[] = [
      { id: "a", lang: "ruby", text: "def x\nend\n" },
      { id: "b", lang: "ruby", text: "def x\nend\n" },
      { id: "c", lang: "rust", text: "fn x() {}\n" },
    ];
    const unique = uniqueHighlightItems(items);
    expect(unique).toHaveLength(2);
    expect(unique.map((u) => u.lang).sort()).toEqual(["ruby", "rust"]);
  });

  it("drops empty text so a spinner never waits on nothing", () => {
    expect(uniqueHighlightItems([{ id: "a", lang: "ruby", text: "" }])).toEqual([]);
  });
});

describe("useHighlight — oversize and batch caps", () => {
  it("drops an oversize item so it cannot 400 the batch", () => {
    const items: HighlightItem[] = [
      { id: "big", lang: "rust", text: OVERSIZE },
      { id: "ok", lang: "rust", text: "fn x() {}\n" },
    ];
    const unique = uniqueHighlightItems(items);
    expect(unique).toHaveLength(1);
    expect(unique[0].text).toBe("fn x() {}\n");
    expect([...skippedHighlightIds(items)]).toEqual(["big"]);
  });

  it("chunks to the server's item-count and byte ceilings", () => {
    const many = Array.from({ length: HIGHLIGHT_BATCH_MAX_ITEMS + 1 }, (_, i) => ({ text: `x${i}` }));
    const byCount = chunkHighlightBatch(many);
    expect(byCount.map((c) => c.length)).toEqual([HIGHLIGHT_BATCH_MAX_ITEMS, 1]);

    const big = "x".repeat(HIGHLIGHT_SNIPPET_MAX_BYTES);
    const need = HIGHLIGHT_BATCH_MAX_BYTES / HIGHLIGHT_SNIPPET_MAX_BYTES + 1;
    const byBytes = chunkHighlightBatch(Array.from({ length: need }, () => ({ text: big })));
    expect(byBytes).toHaveLength(2);
    expect(byBytes[0]).toHaveLength(need - 1);
  });

  it("paints the in-cap sibling and settles the oversize item as unpaintable, not pending", () => {
    const client = new QueryClient();
    const ok = { id: "ok", lang: "rust", text: "fn x() {}\n" };
    const key = highlightCacheKey(ok.lang, ok.text);
    const batch: HighlightBatchOut = {
      schema: "highlight-batch/1",
      items: [
        {
          id: key,
          schema: "highlight/1",
          lang: "rust",
          tier: "full",
          spans: [],
          honesty: { tier: "full", engine: "t", derived_from: "t" },
        },
      ],
    };
    // The batch key carries ONLY the in-cap item: the oversize one is
    // never part of the request.
    client.setQueryData(["highlight", [key]], batch);
    const items: HighlightItem[] = [{ id: "big", lang: "rust", text: OVERSIZE }, ok];
    const box: { r?: ReturnType<typeof useHighlight> } = {};
    function Probe() {
      box.r = useHighlight(items);
      return null;
    }
    renderToStaticMarkup(h(QueryClientProvider, { client }, h(Probe)));
    client.clear();
    expect(box.r?.byId.has("ok")).toBe(true);
    expect(box.r?.byId.has("big")).toBe(false);
    expect(box.r?.unpaintableIds.has("big")).toBe(true);
    expect(box.r?.unpaintableIds.has("ok")).toBe(false);
  });
});
