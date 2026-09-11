// V76-C1 — batched `POST /api/highlight/batch`. Keyed by (lang, text)
// (and path when lang is null). `staleTime: Infinity`: a snippet's paint
// does not go stale; the daemon computes it fresh and stores nothing.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchHighlightBatch } from "../api/client";
import type { HighlightOut } from "../api/types";

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
/// round-trip paints them all.
export function uniqueHighlightItems(items: HighlightItem[]): UniqueHighlight[] {
  const seen = new Set<string>();
  const out: UniqueHighlight[] = [];
  for (const item of items) {
    if (!item.text) continue;
    const key = highlightCacheKey(item.lang, item.text, item.path);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ key, lang: item.lang, text: item.text, path: item.path });
  }
  return out;
}

export function useHighlight(items: HighlightItem[]): {
  byId: Map<string, HighlightOut>;
  isLoading: boolean;
} {
  const unique = useMemo(() => uniqueHighlightItems(items), [items]);
  const q = useQuery({
    queryKey: ["highlight", unique.map((u) => u.key)],
    queryFn: () =>
      fetchHighlightBatch(
        unique.map((u) => ({
          id: u.key,
          lang: u.lang,
          text: u.text,
          path: u.path,
        })),
      ),
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
  return { byId, isLoading: q.isLoading };
}
