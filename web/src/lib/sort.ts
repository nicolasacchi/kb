import type { DocSummary } from "../api/client";

// Sort + group helpers for the gallery. Client-side only; the docs
// API returns up to 200 rows in lance scan order and we re-order in
// the browser. See `gallery.tsx` for the pipeline (filter → sort →
// optionally group).

export type SortKey = "recent" | "indexed" | "created" | "title" | "words";
export type SortDir = "asc" | "desc";
export type GroupKey = "none" | "folder";

/// Default direction per sort key. Title is asc (A→Z), everything
/// else descends (newest / longest first).
export function defaultDir(sort: SortKey): SortDir {
  return sort === "title" ? "asc" : "desc";
}

/// Build a comparator for the given sort key + direction. Missing
/// time fields sort last (the desc default treats them as -Infinity);
/// missing word counts sort as 0.
export function sortComparator(
  sort: SortKey,
  dir: SortDir,
): (a: DocSummary, b: DocSummary) => number {
  const sign = dir === "asc" ? 1 : -1;
  switch (sort) {
    case "recent":
      return (a, b) => sign * (timeOf(a, "mtime") - timeOf(b, "mtime"));
    case "indexed":
      return (a, b) => sign * (timeOf(a, "indexed") - timeOf(b, "indexed"));
    case "created":
      return (a, b) => sign * (timeOf(a, "created") - timeOf(b, "created"));
    case "title":
      return (a, b) => sign * a.title.localeCompare(b.title);
    case "words":
      return (a, b) => sign * ((a.word_count ?? 0) - (b.word_count ?? 0));
  }
}

function timeOf(d: DocSummary, which: "mtime" | "indexed" | "created"): number {
  // Both fields are unix seconds. Treat null/undefined as -Infinity so
  // "no time" rows sink to the bottom of a desc sort (and to the top
  // of an asc one — acceptable since the UI defaults desc on times).
  const v =
    which === "mtime"
      ? d.mtime_unix
      : which === "indexed"
        ? d.indexed_at_unix
        : d.created_unix;
  return v ?? Number.NEGATIVE_INFINITY;
}

/// One section in the grouped-view output.
export type Section = {
  folder: string;
  /// Cards in this section, already sorted by the caller's chosen
  /// comparator.
  docs: DocSummary[];
};

/// Partition a sorted doc list into per-folder sections. Bucket order
/// follows the active sort: for time-based sorts we use each bucket's
/// max time (so the most recently-edited topic floats up); for title
/// we sort buckets alphabetically; for word-count we use each bucket's
/// max word count + the chosen direction. Within a bucket, docs keep
/// their pre-sorted order.
export function groupByFolder(
  sortedDocs: DocSummary[],
  sort: SortKey,
  dir: SortDir,
): Section[] {
  const buckets = new Map<string, DocSummary[]>();
  // Preserve sorted order inside each bucket by inserting in the
  // order docs arrive.
  for (const d of sortedDocs) {
    const key = d.folder ?? "";
    const arr = buckets.get(key);
    if (arr) arr.push(d);
    else buckets.set(key, [d]);
  }
  const sections: Section[] = [];
  for (const [folder, docs] of buckets) sections.push({ folder, docs });
  // Order sections by the active sort criterion applied to each bucket.
  const sign = dir === "asc" ? 1 : -1;
  switch (sort) {
    case "recent":
      sections.sort(
        (a, b) => sign * (maxTime(a.docs, "mtime") - maxTime(b.docs, "mtime")),
      );
      break;
    case "indexed":
      sections.sort(
        (a, b) =>
          sign * (maxTime(a.docs, "indexed") - maxTime(b.docs, "indexed")),
      );
      break;
    case "created":
      sections.sort(
        (a, b) =>
          sign * (maxTime(a.docs, "created") - maxTime(b.docs, "created")),
      );
      break;
    case "title":
      sections.sort((a, b) => sign * a.folder.localeCompare(b.folder));
      break;
    case "words":
      sections.sort((a, b) => sign * (maxWords(a.docs) - maxWords(b.docs)));
      break;
  }
  return sections;
}

function maxTime(
  docs: DocSummary[],
  which: "mtime" | "indexed" | "created",
): number {
  let max = Number.NEGATIVE_INFINITY;
  for (const d of docs) {
    const v =
      which === "mtime"
        ? d.mtime_unix
        : which === "indexed"
          ? d.indexed_at_unix
          : d.created_unix;
    if (v != null && v > max) max = v;
  }
  return max;
}

function maxWords(docs: DocSummary[]): number {
  let max = 0;
  for (const d of docs) {
    const v = d.word_count ?? 0;
    if (v > max) max = v;
  }
  return max;
}
