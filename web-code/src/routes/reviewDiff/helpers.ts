// `ReviewDiff`'s pure helpers (V73-K2b — moved out of `routes/ReviewDiff.tsx`
// verbatim, no behaviour change).
//
// Four total URL-param readers and two small pure functions. They live here
// rather than in `lib/` because they are this ROUTE's own vocabulary
// (`?view=`/`?line=`/`?side=` predate diff v2's `lib/codeUrl.ts` parsers and
// are not part of the shared URL grammar), and here rather than in the route
// file because a pure function that a component file re-exports is a
// function nothing can unit-test without mounting a route.
import type { ReviewFileRow, ReviewReadingStop } from "../../api/types";
import type { DiffMode } from "../../lib/prefs";

export function parseView(raw: string | null): DiffMode | null {
  return raw === "unified" || raw === "split" ? raw : null;
}

export function parseSide(raw: string | null): "old" | "new" | null {
  return raw === "old" || raw === "new" ? raw : null;
}

export function parseLine(raw: string | null): number | null {
  if (raw == null || raw === "") return null;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : null;
}

export function cssAttr(value: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") return CSS.escape(value);
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

export function orderedRows(
  files: ReviewFileRow[],
  stops: ReviewReadingStop[] | null,
): ReviewFileRow[] {
  if (!stops || stops.length === 0) return files;
  const byPath = new Map(files.map((f) => [f.path, f]));
  const seen = new Set<string>();
  const out: ReviewFileRow[] = [];
  for (const s of stops) {
    const f = byPath.get(s.path);
    if (f && !seen.has(f.path)) {
      out.push(f);
      seen.add(f.path);
    }
  }
  for (const f of files) {
    if (!seen.has(f.path)) out.push(f);
  }
  return out;
}

export function msg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * The path a `/diff/*` splat addresses. Each segment is decoded on its own
 * (a segment that is not valid percent-encoding is kept verbatim rather than
 * throwing), empty segments are dropped, and the rest are re-joined.
 */
export function splatPath(splat: string): string {
  if (!splat) return "";
  return splat
    .split("/")
    .map((s) => {
      try {
        return decodeURIComponent(s);
      } catch {
        return s;
      }
    })
    .filter((s) => s !== "")
    .join("/");
}
