// CT-B6 → CT-E4 — the ONE agent-hot-human-cold condition (and its exact
// wording): agents keep getting a memory injected while the human has never
// opened it. Shipped first on the provenance dossier's Attention section
// (CT-B6, ProvenanceThread); CT-E4 reuses it verbatim for the /memory row
// badge and the ?sort=unverified ordering, so the three surfaces can never
// disagree on which rows are "unverified". `read_pct` is the recall wire's
// integer scroll percent — absent and 0 both mean "never meaningfully
// opened", matching the server-side `never_opened_by_human` twin in the
// census's ?sort=unverified (crates/kb-server/src/routes/memory.rs).
// Display/ordering only — never a scoring input (invariant #10's
// surfaced-never-scored posture).

/// The two signals the condition reads, structurally — any RecallHit-shaped
/// row qualifies.
export type AttentionSignals = {
  recall_count: number;
  read_pct?: number | null;
};

/// True when agents keep recalling a memory the human has never opened.
export function isAgentHotHumanCold(r: AttentionSignals): boolean {
  return r.recall_count > 0 && (r.read_pct ?? 0) === 0;
}

/// The CT-B6 tension wording, byte-for-byte.
export function tensionLabel(recallCount: number): string {
  return `Recalled ${recallCount}× by agents · never opened by you`;
}

/// CT-E4 — order rows agent-hot-human-cold FIRST: the bucket leads,
/// recall_count DESC within/after it, and the INPUT order (here: the recall
/// endpoint's score ranking) breaks ties — mirroring the census route's
/// (bucket, recall_count DESC, default-order) key exactly, with each
/// surface keeping its own default order as the final term. Pure: returns a
/// new array, never mutates, drops nothing.
export function orderUnverifiedFirst<T extends AttentionSignals>(rows: readonly T[]): T[] {
  return rows
    .map((r, i) => ({ r, i }))
    .sort((a, b) => {
      const hotA = isAgentHotHumanCold(a.r) ? 0 : 1;
      const hotB = isAgentHotHumanCold(b.r) ? 0 : 1;
      if (hotA !== hotB) return hotA - hotB;
      if (a.r.recall_count !== b.r.recall_count) return b.r.recall_count - a.r.recall_count;
      return a.i - b.i;
    })
    .map(({ r }) => r);
}
