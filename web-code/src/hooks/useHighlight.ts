// V76-C1 — batched `POST /api/highlight/batch`. Keyed by (lang, text)
// (and path when lang is null). `staleTime: Infinity`: a snippet's paint
// does not go stale; the daemon computes it fresh and stores nothing.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchHighlightBatch } from "../api/client";
import type { HighlightBatchOut, HighlightOut } from "../api/types";
import { utf8LengthOf } from "../lib/decorations";

/// Per-SNIPPET ceiling `POST /api/highlight/batch` enforces: one item's
/// `text` may not exceed this many UTF-8 bytes, and a violation refuses the
/// WHOLE batch with a 400 (`MAX_SNIPPET_BYTES` in
/// crates/kb-code-server/src/highlight.rs).
export const HIGHLIGHT_SNIPPET_MAX_BYTES = 256 * 1024;
/// Batch ceilings (`MAX_BATCH_ITEMS`, `MAX_BATCH_BYTES` in highlight.rs).
export const HIGHLIGHT_BATCH_MAX_ITEMS = 64;
export const HIGHLIGHT_BATCH_MAX_BYTES = 1024 * 1024;

/// True when `text` is over the per-snippet ceiling, measured in UTF-8
/// BYTES (the server measures `str::len()`), never UTF-16 `.length`.
export function isOversizeSnippet(text: string): boolean {
  return utf8LengthOf(text, HIGHLIGHT_SNIPPET_MAX_BYTES) > HIGHLIGHT_SNIPPET_MAX_BYTES;
}

export interface HighlightItem {
  id: string;
  lang: string | null;
  text: string;
  path?: string;
}

/// Cache identity for one snippet. The brief names this "sha of (lang,text)";
/// the query key IS that pair (plus path when lang is inferred). A digest
/// would need a sync SHA-256 the SPA does not otherwise ship.
export function highlightCacheKey(lang: string | null, text: string, path?: string): string {
  const inferred = lang ? "" : path ?? "";
  return `${lang ?? ""}|${inferred}|${text}`;
}

export interface UniqueHighlight {
  key: string;
  lang: string | null;
  text: string;
  path?: string;
}

/// Collapse a page of cards onto unique (lang,text) payloads so one batch
/// round-trip paints them all. An item over the per-snippet ceiling is
/// DROPPED (the server would refuse the whole batch for it); the caller
/// learns of it through `skippedHighlightIds`.
export function uniqueHighlightItems(items: HighlightItem[]): UniqueHighlight[] {
  const seen = new Set<string>();
  const out: UniqueHighlight[] = [];
  for (const item of items) {
    if (!item.text) continue;
    if (isOversizeSnippet(item.text)) continue;
    const key = highlightCacheKey(item.lang, item.text, item.path);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ key, lang: item.lang, text: item.text, path: item.path });
  }
  return out;
}

/// Ids of the items that will never be sent: oversize text.
export function skippedHighlightIds(items: HighlightItem[]): Set<string> {
  const out = new Set<string>();
  for (const item of items) if (item.text && isOversizeSnippet(item.text)) out.add(item.id);
  return out;
}

/// Split a unique list into requests within the server's item-count and
/// total-byte ceilings (each element is already within the per-snippet cap).
export function chunkHighlightBatch<T extends { text: string }>(unique: T[]): T[][] {
  const chunks: T[][] = [];
  let cur: T[] = [];
  let bytes = 0;
  for (const u of unique) {
    const n = utf8LengthOf(u.text, HIGHLIGHT_SNIPPET_MAX_BYTES);
    if (
      cur.length > 0 &&
      (cur.length >= HIGHLIGHT_BATCH_MAX_ITEMS || bytes + n > HIGHLIGHT_BATCH_MAX_BYTES)
    ) {
      chunks.push(cur);
      cur = [];
      bytes = 0;
    }
    cur.push(u);
    bytes += n;
  }
  if (cur.length > 0) chunks.push(cur);
  return chunks;
}

export function useHighlight(items: HighlightItem[]): {
  byId: Map<string, HighlightOut>;
  isLoading: boolean;
  /// The batch request failed (a refusal included); nothing will paint.
  isError: boolean;
  /// Item ids that are settled WITHOUT a paint: oversize (skipped) or the
  /// whole request failed. Callers render these "plain", never "pending".
  unpaintableIds: Set<string>;
} {
  const unique = useMemo(() => uniqueHighlightItems(items), [items]);
  const q = useQuery({
    queryKey: ["highlight", unique.map((u) => u.key)],
    queryFn: async (): Promise<HighlightBatchOut> => {
      const settled = await Promise.allSettled(
        chunkHighlightBatch(unique).map((chunk) =>
          fetchHighlightBatch(
            chunk.map((u) => ({ id: u.key, lang: u.lang, text: u.text, path: u.path })),
          ),
        ),
      );
      const ok = settled.filter(
        (r): r is PromiseFulfilledResult<HighlightBatchOut> => r.status === "fulfilled",
      );
      if (ok.length === 0) {
        const bad = settled.find((r) => r.status === "rejected") as PromiseRejectedResult;
        throw bad.reason;
      }
      return { ...ok[0].value, items: ok.flatMap((r) => r.value.items) };
    },
    staleTime: Infinity,
    enabled: unique.length > 0,
  });
  const byId = useMemo(() => {
    const m = new Map<string, HighlightOut>();
    if (!q.data) return m;
    const byKey = new Map(q.data.items.map((i) => [i.id, i]));
    for (const item of items) {
      const key = highlightCacheKey(item.lang, item.text, item.path);
      const hit = byKey.get(key);
      if (hit) m.set(item.id, hit);
    }
    return m;
  }, [q.data, items]);
  const unpaintableIds = useMemo(() => {
    const ids = skippedHighlightIds(items);
    if (q.isError) for (const item of items) if (item.text) ids.add(item.id);
    return ids;
  }, [items, q.isError]);
  return { byId, isLoading: q.isLoading, isError: q.isError, unpaintableIds };
}
