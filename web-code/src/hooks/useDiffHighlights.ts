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
//
// Working-tree tip — a diff rendered with no `to` diffs `from` against the
// WORKING TREE, so the tip query asks for the working tree
// (`ref: undefined`, the same `["file", …, null]` entry `useFile` already
// keeps for the reader's own read) instead of disabling the side and
// leaving every ADDED line plain. The V76-C1 fallback additionally clamps
// each side to the per-snippet ceiling `highlight/batch` enforces: an
// oversize side is dropped rather than sent, because one refused item
// 400s the WHOLE batch and would strip the other side's legitimate paint.

import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchFile } from "../api/client";
import type { FileResponse, HighlightOut } from "../api/types";
import {
  buildLineSpans,
  reconstructSide,
  shouldFallbackToSnippet,
  splitContentLines,
  type DiffHighlights,
  type LineSpan,
} from "../lib/diffHighlight";
import { utf8LengthOf } from "../lib/decorations";
import type { ParsedDiff } from "../lib/diff";
import { loadDiffSyntaxHighlight } from "../lib/prefs";
import { wireSpansToLineMap } from "../lib/paintSpans";
import { useHighlight, type HighlightItem } from "./useHighlight";

/// Same 1.5 MiB ceiling the brief named; `FileResponse.size` is bytes.
export const DIFF_HIGHLIGHT_MAX_BYTES = 1.5 * 1024 * 1024;

/// Per-SNIPPET ceiling `POST /api/highlight/batch` enforces: one item's
/// `text` may not exceed this many UTF-8 bytes, and a violation refuses the
/// WHOLE batch with a 400 (`MAX_SNIPPET_BYTES`,
/// crates/kb-code-server/src/highlight.rs:424, enforced :751-759). It is
/// well under this hook's own `DIFF_HIGHLIGHT_MAX_BYTES`, which only bounds
/// what is worth READING — a side between the two would enqueue an item the
/// server rejects, and the batch's 400 would strip the OTHER side's paint
/// with it. (Two in-cap sides total ≤ 512 KiB, so the batch TOTAL cap of
/// 1 MiB — `highlight.rs:428`, enforced :761-765 — is unreachable once
/// each side is clamped.)
export const HIGHLIGHT_SNIPPET_MAX_BYTES = 256 * 1024;

export interface UseDiffHighlightsOpts {
  /// Base (`oldSha`) is fetched ONLY when the parsed diff has remove
  /// lines — an add-only file has no pre-image text to paint.
  hasRemoves?: boolean;
  /// Tip (`newSha`) — the post-image — is fetched whenever the diff has
  /// add lines, INCLUDING the working-tree read a no-`to` diff makes (see
  /// `tipSideEnabled`). Unset/omitted means "assume it has adds", so only
  /// an explicit `false` — the deleted file — closes the gate.
  hasAdds?: boolean;
  /// When file spans are missing, reconstruct a side from this parsed
  /// diff and POST it to highlight/1.
  parsed?: ParsedDiff | null;
}

/// Tip-side fetch gate. The tip ref is `newSha`, and it is ABSENT whenever
/// the diff is rendered without a `to` — which per `GET /api/diff`'s
/// contract (`crates/kb-code-server/src/routes.rs:2086-2090`) is exactly
/// the working tree, i.e. the new side's real content, not "nothing to
/// read". So the gate asks for that working tree rather than dropping the
/// side and painting every ADDED line plain.
///
/// `hasAdds` is the one gate that survives, and only for the UNPINNED read:
/// a deleted file has no working-tree blob, so the read would be a
/// guaranteed 404 whose only effect is a wasted request
/// (`shouldFallbackToSnippet` refuses on a failed read, so nothing would
/// paint). Same symmetric line-mix gate `hasRemoves` gives the base side.
/// A PINNED tip is never gated on `hasAdds` — that blob exists whatever the
/// diff's line mix says, which is today's behaviour, unchanged.
export function tipSideEnabled(side: {
  prefOn: boolean;
  repo: string | undefined;
  path: string | undefined;
  newSha: string | undefined;
  hasAdds: boolean;
}): boolean {
  return (
    side.prefOn &&
    side.repo !== undefined &&
    side.path !== undefined &&
    (!!side.newSha || side.hasAdds)
  );
}

/// Base-side fetch gate — untouched by the tip fix, and extracted beside it
/// so both rules read (and are pinned) side by side. The base is only ever
/// read at a pinned `oldSha`, so BOTH the ref and the line-mix gate apply.
export function baseSideEnabled(side: {
  prefOn: boolean;
  repo: string | undefined;
  path: string | undefined;
  oldSha: string | undefined;
  hasRemoves: boolean;
}): boolean {
  return (
    side.prefOn &&
    side.repo !== undefined &&
    side.path !== undefined &&
    !!side.oldSha &&
    side.hasRemoves
  );
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
  const hasAdds = opts.hasAdds !== false;
  const parsed = opts.parsed ?? null;
  const { oldSha, newSha } = shas;

  const tipEnabled = tipSideEnabled({ prefOn, repo, path, newSha, hasAdds });
  const baseEnabled = baseSideEnabled({ prefOn, repo, path, oldSha, hasRemoves });

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
      tipEnabled &&
      shouldFallbackToSnippet({ isLoading: tip.isLoading, isError: tip.isError }, sideUsable(tip.data));
    if (tipNeeds) {
      const text =
        tip.data?.encoding === "utf8" && tip.data.content
          ? tip.data.content
          : parsed
            ? reconstructSide(parsed, "new")
            : null;
      // An oversize side is DROPPED, not sent: the server refuses the whole
      // batch, and the OTHER side has a legitimate paint to lose. UTF-8
      // BYTES, never `.length` — the server measures `item.text.len()`, so
      // a non-ASCII side under-counts on a UTF-16 length. The cap is handed
      // to `utf8LengthOf` so a side long enough to be over it is settled by
      // one comparison instead of encoding a 1.5 MiB copy of itself to
      // learn a number.
      if (text && utf8LengthOf(text, HIGHLIGHT_SNIPPET_MAX_BYTES) <= HIGHLIGHT_SNIPPET_MAX_BYTES) {
        items.push({
          id: "new",
          lang: tip.data?.lang ?? null,
          text,
          path,
        });
      }
    }
    const baseNeeds =
      baseEnabled &&
      shouldFallbackToSnippet({ isLoading: base.isLoading, isError: base.isError }, sideUsable(base.data));
    if (baseNeeds) {
      const text =
        base.data?.encoding === "utf8" && base.data.content
          ? base.data.content
          : parsed
            ? reconstructSide(parsed, "old")
            : null;
      if (text && utf8LengthOf(text, HIGHLIGHT_SNIPPET_MAX_BYTES) <= HIGHLIGHT_SNIPPET_MAX_BYTES) {
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
    tip.isError,
    base.isLoading,
    base.isError,
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
