// CT-B3 — the pure grouping/cap logic behind every "?ids= gallery pivot"
// chip: every cross-entity list that already resolves to (kb, id) pairs
// (a session's touched files, its recalled memories, a memory's lineage
// chain) feeds through this to build one chip PER KB (a chip only ever
// links into ONE gallery, and the gallery is itself kb-scoped, invariant
// #33), cap-aware per invariant #35's degrade-LOUDLY rule (the `ids=`
// atom is a HARD 500-id cap, both client- and server-side — never a
// silent truncation).
//
// Extracted here (rather than inlined per-component) so it's unit-testable
// without mounting React — the components in `routes/sessions.tsx` and
// `components/LineageViewer.tsx` call straight through to this.

/// Mirrors the server/SPA `?ids=` hard cap (invariant #35, W2.3a).
export const GALLERY_IDS_CAP = 500;

export type KbIdPair = { kb: string; id: string };

export type IdsPivotGroup = {
  kb: string;
  /// De-duplicated, first-seen order.
  ids: string[];
};

/// Groups `(kb, id)` pairs by kb. Group order = first-seen kb order; id
/// order within a group = first-seen order, deduplicated (a file read
/// twice, or a memory recalled at two turns, must not double-count towards
/// the cap or repeat in the `ids=` csv). Pairs missing either field are
/// dropped — a chip can't link to half an artifact.
export function groupIdsByKb(pairs: readonly KbIdPair[]): IdsPivotGroup[] {
  const kbOrder: string[] = [];
  const byKb = new Map<string, Set<string>>();
  for (const { kb, id } of pairs) {
    if (!kb || !id) continue;
    let ids = byKb.get(kb);
    if (!ids) {
      ids = new Set();
      byKb.set(kb, ids);
      kbOrder.push(kb);
    }
    ids.add(id);
  }
  return kbOrder.map((kb) => ({ kb, ids: Array.from(byKb.get(kb)!) }));
}

/// A group at/over the cap can't be a working `?ids=` pivot at all (the
/// gallery route would 400 it) — the caller renders a disabled chip with
/// an explicit reason instead of silently trimming the id set.
export function isOverIdsCap(ids: readonly string[]): boolean {
  return ids.length > GALLERY_IDS_CAP;
}
