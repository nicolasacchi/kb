// v0.33 Y4 — convention: a folder's note is exactly `<folder>/index.md`
// (root → `index.md`). Pure detection so gallery + Folder browser stay in
// lock-step; both call sites use this helper, never ad-hoc path joins.

/** Exact `source_relative` of the convention folder note for `folder`.
 *  Nested: `${folder}/index.md`. Root (`""`): `index.md`. */
export function folderIndexNoteRel(folder: string): string {
  return folder ? `${folder}/index.md` : "index.md";
}

/** Find the row whose `source_relative` is exactly the folder's index note.
 *  `folder == null` means "no folder scope" (gallery without `?folder=`) and
 *  always returns null. Pass `""` for the kb root. */
export function folderIndexNote<T extends { source_relative: string }>(
  rows: readonly T[],
  folder: string | null | undefined,
): T | null {
  if (folder == null) return null;
  const target = folderIndexNoteRel(folder);
  for (const r of rows) {
    if (r.source_relative === target) return r;
  }
  return null;
}
