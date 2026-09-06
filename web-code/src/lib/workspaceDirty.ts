// V70-A10 ("Workspaces v0", D26) — pure comparison between the LIVE
// working set (an ordered list of repo-relative paths, `lib/workingSet.ts`'s
// own `WorkingSetEntry[]`) and a workspace's own SAVED entries (its
// `SetView.spans`, path-only) — "a dirty marker when the open file set
// differs from the saved entries" (D26's own stated grammar). Line/ref/note
// drift on an unchanged path set is deliberately NOT dirty — only the file
// SET/ORDER is; a workspace note or a re-read line number isn't something
// "Save workspace" itself would ever re-capture on its own (the composer
// re-reads live cursor positions at save time regardless).

/// `true` iff the two ordered path lists differ in length, membership, or
/// order. Pure, total (no `null`/`undefined` special-casing needed — an
/// empty working set against a non-empty saved one is honestly dirty, and
/// vice versa).
export function isWorkingSetDirty(liveOrder: string[], savedOrder: string[]): boolean {
  if (liveOrder.length !== savedOrder.length) return true;
  return liveOrder.some((p, i) => p !== savedOrder[i]);
}
