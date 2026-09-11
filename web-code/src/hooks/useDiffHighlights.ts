// V4.D2 — per-side highlight maps for a parsed diff. Two `useFile`-shaped
// queries share the reader's exact cache key (`["file", repo, path, ref ??
// null]`) so a commit-page expand of a file the reader already has open
// is a cache hit. Degrades to `null` (plain text) while loading, when
// the pref is off, or when a side is unusable.
//
// V76-C1 — when the file's stored spans are unavailable (new files,
// unindexed blobs, pseudo-files, interdiff snippets) fall back to
// `POST /api/highlight` over the file content or a reconstructed hunk
// side. The per-line integrity guard is unchanged.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchFile } from "../api/client";
import type { FileResponse, HighlightOut } from "../api/types";
import {
  buildLineSpans,
  reconstructSide,
  splitContentLines,
  type DiffHighlights,
  type LineSpan,
} from "../lib/diffHighlight";
import type { ParsedDiff } from "../lib/diff";
import { loadDiffSyntaxHighlight } from "../lib/prefs";
import { wireSpansToLineMap } from "../lib/paintSpans";
import { useHighlight, type HighlightItem } from "./useHighlight";

/// Same 1.5 MiB ceiling the brief named; `FileResponse.size` is bytes.
export const DIFF_HIGHLIGHT_MAX_BYTES = 1.5 * 1024 * 1024;

export interface UseDiffHighlightsOpts {
  /// Base (`oldSha`) is fetched ONLY when the parsed diff has remove
  /// lines — an add-only file has no pre-image text to paint.
  hasRemoves?: boolean;
  /// When file spans are missing, reconstruct a side from this parsed
  /// diff and POST it to highlight/1.
  parsed?: ParsedDiff | null;
}

/// EXACT key `useFile` uses — do not drift the 4-tuple or `ref ?? null`.
function fileQueryKey(repo: string | undefined, path: string | undefined, ref: string | undefined) {
  return ["file", repo, path, ref ?? null] as const;
}

function sideUsable(file: FileResponse | undefined): file is FileResponse {
  if (!file) return false;
  if (file.encoding !== "utf8") return false;
  if (file.size > DIFF_HIGHLIGHT_MAX_BYTES) return false;
  if (file.highlights == null) return false;
  return true;
}

function sideFromFile(file: FileResponse | undefined): {
  spans: Map<number, LineSpan[]>;
  lines: string[] | null;
} {
  if (!sideUsable(file) || file.highlights == null) return { spans: new Map(), lines: null };
  return {
    spans: buildLineSpans(file.content, file.highlights),
    lines: splitContentLines(file.content),
  };
}

function sideFromSnippet(
  text: string | null,
  painted: HighlightOut | undefined,
): { spans: Map<number, LineSpan[]>; lines: string[] | null } {
  if (!text || !painted || painted.tier === "none") {
    return { spans: new Map(), lines: text ? splitContentLines(text) : null };
  }
  return {
    spans: wireSpansToLineMap(text, painted.spans),
    lines: splitContentLines(text),
  };
}

export function useDiffHighlights(
  repo: string | undefined,
  path: string | undefined,
  shas: { oldSha?: string; newSha?: string },
  opts: UseDiffHighlightsOpts = {},
): DiffHighlights | null {
  const prefOn = loadDiffSyntaxHighlight();
  const hasRemoves = opts.hasRemoves === true;
  const parsed = opts.parsed ?? null;
  const { oldSha, newSha } = shas;

  const tipEnabled = prefOn && repo !== undefined && path !== undefined && !!newSha;
  const baseEnabled = prefOn && repo !== undefined && path !== undefined && !!oldSha && hasRemoves;

  const tip = useQuery({
    queryKey: fileQueryKey(repo, path, newSha),
    queryFn: () => fetchFile(repo as string, path as string, newSha),
    enabled: tipEnabled,
  });
  const base = useQuery({
    queryKey: fileQueryKey(repo, path, oldSha),
    queryFn: () => fetchFile(repo as string, path as string, oldSha),
    enabled: baseEnabled,
  });

  const fallbackItems: HighlightItem[] = useMemo(() => {
    if (!prefOn || !path) return [];
    const items: HighlightItem[] = [];
    const tipNeeds =
      tipEnabled && !tip.isLoading && !sideUsable(tip.data);
    if (tipNeeds) {
      const text =
        tip.data?.encoding === "utf8" && tip.data.content
          ? tip.data.content
          : parsed
            ? reconstructSide(parsed, "new")
            : null;
      if (text) {
        items.push({
          id: "new",
          lang: tip.data?.lang ?? null,
          text,
          path,
        });
      }
    }
    const baseNeeds =
      baseEnabled && !base.isLoading && !sideUsable(base.data);
    if (baseNeeds) {
      const text =
        base.data?.encoding === "utf8" && base.data.content
          ? base.data.content
          : parsed
            ? reconstructSide(parsed, "old")
            : null;
      if (text) {
        items.push({
          id: "old",
          lang: base.data?.lang ?? null,
          text,
          path,
        });
      }
    }
    return items;
  }, [
    prefOn,
    path,
    tipEnabled,
    baseEnabled,
    tip.isLoading,
    base.isLoading,
    tip.data,
    base.data,
    parsed,
  ]);

  const snippet = useHighlight(fallbackItems);

  return useMemo(() => {
    if (!prefOn) return null;
    if (tipEnabled && tip.isLoading) return null;
    if (baseEnabled && base.isLoading) return null;
    const neuFile = sideFromFile(tip.data);
    const oldFile = sideFromFile(base.data);
    const neuFb = fallbackItems.find((i) => i.id === "new");
    const oldFb = fallbackItems.find((i) => i.id === "old");
    const neu =
      neuFile.lines != null
        ? neuFile
        : sideFromSnippet(neuFb?.text ?? null, snippet.byId.get("new"));
    const old =
      oldFile.lines != null
        ? oldFile
        : sideFromSnippet(oldFb?.text ?? null, snippet.byId.get("old"));
    return {
      oldLineSpans: old.spans,
      newLineSpans: neu.spans,
      oldLines: old.lines,
      newLines: neu.lines,
    };
  }, [
    prefOn,
    tipEnabled,
    baseEnabled,
    tip.isLoading,
    base.isLoading,
    tip.data,
    base.data,
    fallbackItems,
    snippet.byId,
  ]);
}
