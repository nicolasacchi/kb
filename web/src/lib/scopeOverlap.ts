// MI-W4.7 — pure UpSet-style set-intersection aggregation over the memory
// population's cross-kb scope (`global`/`linked_kbs` per `RecallHit`/
// `MemoryCensusRow` — invariant "every memory carries global:bool +
// linked_kbs[]"). No React, no fetch — colocated `scopeOverlap.test.ts`
// covers it with plain vitest.
//
// A memory's SCOPE SET is either:
//   - global:  visible to every kb (rendered as its own "★ global" row —
//     NOT decomposed into member kbs, which would be misleadingly literal
//     for "everywhere").
//   - otherwise: the sorted-unique union of its home kb (`kb`) + its
//     explicit `linked_kbs` — e.g. a memory homed in "alpha" and linked to
//     "beta" groups under the SAME "alpha+beta" row as one homed in "beta"
//     and linked to "alpha": the combination is what matters, not which
//     side is "home".

export type ScopeOverlapInput = {
  kb: string;
  global: boolean;
  linked_kbs: string[];
};

export type ScopeOverlapRow = {
  /// Stable identity: `"*"` for the global row, else members joined by `+`.
  key: string;
  /// For a combo row: the memory's home kb + its links, sorted asc. For the
  /// global row: the HOME kbs of the global memories grouped here
  /// (informational — global memories are visible everywhere regardless).
  members: string[];
  isGlobal: boolean;
  count: number;
};

function comboKey(members: string[]): string {
  return members.join("+");
}

/// Group `hits` into `ScopeOverlapRow`s. Deterministic order: the global row
/// first (when present), then by count descending, ties broken by `key`
/// ascending — so re-renders (and this module's own golden tests) are
/// stable regardless of input order.
export function aggregateScopeOverlap(hits: ScopeOverlapInput[]): ScopeOverlapRow[] {
  const globalMembers = new Set<string>();
  let globalCount = 0;
  const combos = new Map<string, { members: string[]; count: number }>();

  for (const h of hits) {
    if (h.global) {
      globalCount += 1;
      globalMembers.add(h.kb);
      continue;
    }
    const members = Array.from(new Set([h.kb, ...h.linked_kbs])).sort();
    const key = comboKey(members);
    const existing = combos.get(key);
    if (existing) existing.count += 1;
    else combos.set(key, { members, count: 1 });
  }

  const rows: ScopeOverlapRow[] = [];
  if (globalCount > 0) {
    rows.push({
      key: "*",
      members: Array.from(globalMembers).sort(),
      isGlobal: true,
      count: globalCount,
    });
  }
  for (const [key, v] of combos) {
    rows.push({ key, members: v.members, isGlobal: false, count: v.count });
  }

  return rows.sort((a, b) => {
    if (a.isGlobal !== b.isGlobal) return a.isGlobal ? -1 : 1;
    if (b.count !== a.count) return b.count - a.count;
    return a.key.localeCompare(b.key);
  });
}

/// Every distinct kb name touched by any row (union of every row's
/// `members`), sorted asc — the UpSet dot-matrix's column headers.
export function scopeOverlapColumns(rows: ScopeOverlapRow[]): string[] {
  const s = new Set<string>();
  for (const r of rows) for (const m of r.members) s.add(m);
  return Array.from(s).sort();
}

/// The kb a row's click should pivot `?kb=` to (the existing `forKb` lens
/// plumbing only accepts ONE kb name) — the alphabetically-first member.
/// `null` when the row has no members at all (defensive; every real row
/// with `count > 0` has at least one).
export function scopeOverlapPivotKb(row: ScopeOverlapRow): string | null {
  return row.members[0] ?? null;
}
