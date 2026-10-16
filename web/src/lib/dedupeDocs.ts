// One artifact, one card. The docs list is merged from offset-paged
// responses, and a page boundary (or a daemon that still holds two rows for
// one artifact id) can hand the same artifact back twice. Rendering both would
// also reuse React elements under a duplicate key, so the list is deduped by
// (kb, id) where the pages are merged, keeping the FIRST occurrence so the
// server's sort order is preserved.
export interface HasDocId {
  id: string;
}

export function docKey(kb: string | null, id: string): string {
  return `${kb ?? ""}:${id}`;
}

export function dedupeDocRows<T extends HasDocId>(
  kb: string | null,
  rows: readonly T[],
): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const r of rows) {
    const k = docKey(kb, r.id);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(r);
  }
  return out;
}
