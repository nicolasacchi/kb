// The diff-highlight hook's two gates and its batch clamp.
//
// No DOM anywhere: `renderToStaticMarkup` + a real `QueryClientProvider` is
// this repo's established way to pin a hook's RETURN value under
// `environment: "node"` (`hooks/useReviewMutationsAdmitted.test.ts`'s
// probe, `components/prose/ProseBlock.test.ts`'s seeded cache), so every
// case here drives the actual `useDiffHighlights` and asserts on what it
// returns. SSR runs NO effects, so an enabled query never fetches and a
// DISABLED one still reads a seeded cache entry — which means the render
// half of a case can prove the DATA path (spans/lines/batch payload) but
// cannot by itself prove a query would fire. The gate half is pinned on the
// exported pure predicates, which is the part that actually regressed: with
// no `to` the tip query used to be disabled, and every ADDED line rendered
// plain. `batchKeys` is read off the query cache: `useHighlight` keys its
// `POST /api/highlight/batch` query on the exact payload, and react-query
// BUILDS that query during render (`QueryObserver.getOptimisticResult`),
// so the batch's contents are observable without a network call.

import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import type { FileResponse } from "../api/types";
import type { DiffHighlights } from "../lib/diffHighlight";
import {
  baseSideEnabled,
  HIGHLIGHT_SNIPPET_MAX_BYTES,
  tipSideEnabled,
  useDiffHighlights,
  type UseDiffHighlightsOpts,
} from "./useDiffHighlights";

const REPO = "r";
const PATH = "p.ts";

/// The EXACT key `useFile` uses; `null` IS the working tree — which is what
/// a no-`to` diff's new side reads.
function fileKey(ref: string | null) {
  return ["file", REPO, PATH, ref] as const;
}

const CONTENT = "const x = 1;\n";
const SMALL = "const y = 2;\n";

/// One CJK scalar is 3 UTF-8 bytes and ONE UTF-16 unit, so this sits just
/// OVER the server's per-item cap (`MAX_SNIPPET_BYTES`, `highlight.rs:424`)
/// while staying UNDER it as a `.length` — the case a UTF-16 comparison
/// waves through and the server refuses, 400-ing the whole batch.
const OVERSIZE = "漢".repeat(Math.floor(HIGHLIGHT_SNIPPET_MAX_BYTES / 3) + 1);

function fileResponse(ref: string | null, content: string, highlights: FileResponse["highlights"]): FileResponse {
  return {
    repo: REPO,
    path: PATH,
    ref,
    size: content.length,
    blob_hash: `h-${ref ?? "wt"}-${content.length}`,
    lang: "ts",
    encoding: "utf8",
    content,
    symbols: [],
    highlights,
  };
}

function renderHook(args: {
  shas: { oldSha?: string; newSha?: string };
  opts?: UseDiffHighlightsOpts;
  seed?: ReadonlyArray<readonly [readonly unknown[], FileResponse]>;
}): { result: DiffHighlights | null; batchKeys: string[] } {
  const client = new QueryClient();
  for (const [key, data] of args.seed ?? []) client.setQueryData(key, data);
  const box: { result: DiffHighlights | null } = { result: null };
  function Probe() {
    box.result = useDiffHighlights(REPO, PATH, args.shas, args.opts);
    return null;
  }
  renderToStaticMarkup(h(QueryClientProvider, { client }, h(Probe)));
  const batchKeys = client
    .getQueryCache()
    .findAll({ queryKey: ["highlight"] })
    .flatMap((q) => (q.queryKey[1] as string[] | undefined) ?? []);
  client.clear();
  return { result: box.result, batchKeys };
}

describe("useDiffHighlights — the tip side of a diff with no `to`", () => {
  it("reads the new side as the WORKING TREE and its spans are reachable", () => {
    // The defect: `!!newSha` made the tip side unreachable for every diff
    // rendered without a `to` — the new side's spans, its lines, its
    // snippet, all dropped, every ADDED line plain.
    expect(tipSideEnabled({ prefOn: true, repo: REPO, path: PATH, newSha: undefined, hasAdds: true })).toBe(
      true,
    );

    // …and given that read, the stored spans reach the render.
    const { result } = renderHook({
      shas: { oldSha: "aaa" },
      opts: { hasRemoves: false, hasAdds: true },
      seed: [[fileKey(null), fileResponse(null, CONTENT, [{ byte_start: 0, byte_len: 6, class: "keyword" }])]],
    });
    expect(result).toEqual({
      oldLineSpans: new Map(),
      newLineSpans: new Map([[1, [{ start: 0, end: 6, cls: "kbc-hl-keyword" }]]]),
      oldLines: null,
      newLines: ["const x = 1;"],
    });
  });
});

describe("useDiffHighlights — the hasAdds gate (deleted file)", () => {
  it("closes the WORKING-TREE tip read for a deleted file, and nothing else", () => {
    // No `to` + no add lines ⇒ no working-tree blob ⇒ a guaranteed 404 whose
    // only effect is a wasted request, so the side is never read.
    expect(tipSideEnabled({ prefOn: true, repo: REPO, path: PATH, newSha: undefined, hasAdds: false })).toBe(
      false,
    );
    // A PINNED tip is untouched: that blob exists whatever the diff's line
    // mix says, which is today's behaviour.
    expect(tipSideEnabled({ prefOn: true, repo: REPO, path: PATH, newSha: "bbb", hasAdds: false })).toBe(true);
    // Omitted means "assume it has adds" — the gate is opt-OUT.
    expect(tipSideEnabled({ prefOn: true, repo: REPO, path: PATH, newSha: undefined, hasAdds: true })).toBe(
      true,
    );
    // The pref still wins over everything.
    expect(tipSideEnabled({ prefOn: false, repo: REPO, path: PATH, newSha: "bbb", hasAdds: true })).toBe(false);
  });

  it("degrades a deleted file's new side to plain, with no snippet to wait on", () => {
    const { result, batchKeys } = renderHook({
      shas: { oldSha: "aaa" },
      opts: { hasRemoves: false, hasAdds: false },
    });
    expect(result).not.toBeNull();
    expect(result?.newLines).toBeNull();
    expect(result?.newLineSpans.size).toBe(0);
    expect(batchKeys).toEqual([]);
  });
});

describe("useDiffHighlights — the hasRemoves gate is untouched", () => {
  it("leaves the base side disabled for an add-only diff", () => {
    expect(baseSideEnabled({ prefOn: true, repo: REPO, path: PATH, oldSha: "aaa", hasRemoves: false })).toBe(
      false,
    );
    expect(baseSideEnabled({ prefOn: true, repo: REPO, path: PATH, oldSha: "aaa", hasRemoves: true })).toBe(
      true,
    );
    // Still ref-gated, still pref-gated — the tip fix changed neither.
    expect(baseSideEnabled({ prefOn: true, repo: REPO, path: PATH, oldSha: undefined, hasRemoves: true })).toBe(
      false,
    );
    expect(baseSideEnabled({ prefOn: false, repo: REPO, path: PATH, oldSha: "aaa", hasRemoves: true })).toBe(
      false,
    );
  });
});

describe("useDiffHighlights — one oversize side must not strip the other", () => {
  it("drops the oversize fallback item and keeps its in-cap sibling in the batch", () => {
    // `highlight/batch` refuses the WHOLE batch when one item is over
    // `MAX_SNIPPET_BYTES`, so enqueueing the tip side would cost the base
    // side its legitimate paint. Both sides are stored-span-less here
    // (`highlights: null`), which is exactly the state that arms the
    // fallback.
    const { batchKeys } = renderHook({
      shas: { oldSha: "aaa", newSha: "bbb" },
      opts: { hasRemoves: true, hasAdds: true },
      seed: [
        [fileKey("bbb"), fileResponse("bbb", OVERSIZE, null)],
        [fileKey("aaa"), fileResponse("aaa", SMALL, null)],
      ],
    });
    expect(batchKeys).toHaveLength(1);
    expect(batchKeys[0].endsWith(SMALL)).toBe(true);
    expect(batchKeys[0].includes(OVERSIZE)).toBe(false);
  });

  it("still enqueues BOTH sides when both are within the cap", () => {
    const { batchKeys } = renderHook({
      shas: { oldSha: "aaa", newSha: "bbb" },
      opts: { hasRemoves: true, hasAdds: true },
      seed: [
        [fileKey("bbb"), fileResponse("bbb", CONTENT, null)],
        [fileKey("aaa"), fileResponse("aaa", SMALL, null)],
      ],
    });
    expect(batchKeys).toHaveLength(2);
  });
});

describe("useDiffHighlights — pref off", () => {
  it("returns null and enqueues nothing", () => {
    // `loadPrefs()` swallows an unavailable `localStorage` and falls back to
    // the defaults (pref ON) — which is why every other case above can leave
    // storage unstubbed. Here the pref is explicitly off, so stub it the way
    // `lib/prefs.test.ts` does.
    const store: Record<string, string> = {
      "kbc:prefs": JSON.stringify({ diffSyntaxHighlight: false }),
    };
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => (k in store ? store[k] : null),
      setItem: () => {},
      removeItem: () => {},
      clear: () => {},
    });
    try {
      const { result, batchKeys } = renderHook({
        shas: { oldSha: "aaa", newSha: "bbb" },
        opts: { hasRemoves: true, hasAdds: true },
        seed: [[fileKey("bbb"), fileResponse("bbb", CONTENT, [{ byte_start: 0, byte_len: 6, class: "keyword" }])]],
      });
      expect(result).toBeNull();
      expect(batchKeys).toEqual([]);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
