// V4.D2 — per-side highlight maps for a parsed diff. Two `useFile`-shaped
// queries share the reader's exact cache key (`["file", repo, path, ref ??
// null]`) so a commit-page expand of a file the reader already has open
// is a cache hit. Degrades to `null` (plain text) while loading, when
// the pref is off, or when a side is unusable.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchFile } from "../api/client";
import type { FileResponse } from "../api/types";
import {
  buildLineSpans,
  splitContentLines,
  type DiffHighlights,
  type LineSpan,
} from "../lib/diffHighlight";
import { loadDiffSyntaxHighlight } from "../lib/prefs";

/// Same 1.5 MiB ceiling the brief named; `FileResponse.size` is bytes.
export const DIFF_HIGHLIGHT_MAX_BYTES = 1.5 * 1024 * 1024;

export interface UseDiffHighlightsOpts {
  /// Base (`oldSha`) is fetched ONLY when the parsed diff has remove
  /// lines — an add-only file has no pre-image text to paint.
  hasRemoves?: boolean;
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

export function useDiffHighlights(
  repo: string | undefined,
  path: string | undefined,
  shas: { oldSha?: string; newSha?: string },
  opts: UseDiffHighlightsOpts = {},
): DiffHighlights | null {
  const prefOn = loadDiffSyntaxHighlight();
  const hasRemoves = opts.hasRemoves === true;
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

  return useMemo(() => {
    if (!prefOn) return null;
    // Wait for every *enabled* side. Disabled / errored sides degrade
    // (empty maps) rather than blocking the other. No toast — a 404 is
    // "paint plain", not a user-facing failure.
    if (tipEnabled && tip.isLoading) return null;
    if (baseEnabled && base.isLoading) return null;
    const neu = sideFromFile(tip.data);
    const old = sideFromFile(base.data);
    return {
      oldLineSpans: old.spans,
      newLineSpans: neu.spans,
      oldLines: old.lines,
      newLines: neu.lines,
    };
  }, [prefOn, tipEnabled, baseEnabled, tip.isLoading, base.isLoading, tip.data, base.data]);
}
