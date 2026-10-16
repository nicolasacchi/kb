/** True when at least one hit has data for the Memory table's HEALTH column
 *  (the decay projection + injection histogram only render for a memory that
 *  has been recalled at least once). When false the column is dropped. */
export function anyHealth(hits: readonly { recall_count: number }[]): boolean {
  return hits.some((h) => h.recall_count >= 1);
}
