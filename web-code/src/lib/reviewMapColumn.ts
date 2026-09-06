// V73-K2a — the file-map COLUMN's own pure half (design §D9: "map as grid
// columns"; Track K's diff-v2 MUST list opens with "map as columns").
//
// The column groups the review's files into CHAPTERS. The wire does not
// carry chapters: `GET /reviews/{id}/reading-order` returns a FLAT list of
// `{path, reason, cycle}` stops (`review_map.rs`'s `ReadingStop`), where
// `reason` is a short free-text sentence the daemon computed
// deterministically ("imported by 1 changed files", "deleted", "test", "no
// dependency signal"). Authored chapters are a Track-K SHOULD and are NOT
// built here.
//
// So the chapters are DERIVED, by grouping consecutive stops that share a
// reason, and the column says so on screen ("chapters derived from the
// reading order's own reasons"). Two consequences, both deliberate:
//
//   * grouping is over CONSECUTIVE stops, never a global bucket-by-reason.
//     The reading order is a topological walk; re-bucketing it would
//     silently reorder the one thing the wire is actually asserting.
//   * a file the reading order does not mention (a fresh fetch, an
//     `inputs_missing` degrade) lands in a final, explicitly-named group
//     rather than being dropped — the same "absence is stated, never
//     zeroed" rule the tree's honesty strip follows.

import type { ReviewFileRow, ReviewReadingStop } from "../api/types";

export interface MapChapter {
  /// The shared `reason`, verbatim from the wire — or `null` for the
  /// trailing "not in the reading order" group, which names itself.
  reason: string | null;
  files: ReviewFileRow[];
}

/// Group `ordered` (files already in reading-order sequence — the route's
/// own `orderedRows`) into consecutive-reason chapters using `stops`.
/// `stops === null` (the route has no reading order at all) yields ONE
/// unnamed chapter holding everything, which renders as a plain list.
export function mapChapters(
  ordered: readonly ReviewFileRow[],
  stops: readonly ReviewReadingStop[] | null,
): MapChapter[] {
  if (!stops || stops.length === 0) {
    return ordered.length === 0 ? [] : [{ reason: null, files: [...ordered] }];
  }
  const reasonByPath = new Map<string, string>();
  for (const s of stops) if (!reasonByPath.has(s.path)) reasonByPath.set(s.path, s.reason);

  const out: MapChapter[] = [];
  const unordered: ReviewFileRow[] = [];
  for (const f of ordered) {
    const reason = reasonByPath.get(f.path);
    if (reason === undefined) {
      unordered.push(f);
      continue;
    }
    const last = out[out.length - 1];
    if (last && last.reason === reason) last.files.push(f);
    else out.push({ reason, files: [f] });
  }
  if (unordered.length > 0) out.push({ reason: null, files: unordered });
  return out;
}

/// The state chips one map row shows. Every field is a COUNT of something
/// on the wire or on screen — nothing here is a ranking, and the row never
/// re-derives a number the server already sent (`ReviewFileRow.viewed` /
/// `open_annotations` are used verbatim).
export interface MapRowState {
  path: string;
  viewed: boolean;
  viewedStale: boolean;
  openComments: number;
  findings: number;
  drafts: number;
  /// Noise classes on the FILE (never on its hunks — a map row cannot
  /// know a hunk it has not loaded, and claiming otherwise would be the
  /// partial-index dishonesty `diffNoise.ts` warns about).
  noise: string[];
}

/// A one-line, human-readable summary of a row's state — the row's own
/// `title`, so hovering says exactly what the chips mean.
export function mapRowTitle(s: MapRowState): string {
  const parts: string[] = [s.path];
  parts.push(s.viewed ? (s.viewedStale ? "viewed (stale — the blob changed)" : "viewed") : "unviewed");
  if (s.openComments > 0) parts.push(`${s.openComments} open comment${s.openComments === 1 ? "" : "s"}`);
  if (s.findings > 0) parts.push(`${s.findings} finding${s.findings === 1 ? "" : "s"}`);
  if (s.drafts > 0) parts.push(`${s.drafts} unpublished draft${s.drafts === 1 ? "" : "s"}`);
  if (s.noise.length > 0) parts.push(`noise: ${s.noise.join(", ")}`);
  return parts.join(" · ");
}

/// The column header's census: files, viewed, chapters. Deliberately
/// separate from the toolbar's own `viewedCount/filesCount` pair rather
/// than a second computation of it — the caller passes both in.
export function mapCensusText(files: number, viewed: number, chapters: number): string {
  const ch = chapters === 1 ? "1 chapter" : `${chapters} chapters`;
  return `${files} file${files === 1 ? "" : "s"} · ${viewed} viewed · ${ch}`;
}
