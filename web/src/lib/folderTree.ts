// Pure helpers over the GET /api/kb/{kb}/folders tree (FolderNode[]).
// Used by the reader Folder tab browser (F2) so drill-down never reimplements
// path-walk logic in the component.

export type FolderTreeNode = {
  path: string;
  count: number;
  children: FolderTreeNode[];
};

export type ChildFolder = {
  /** Leaf segment of `path` (e.g. "notes" for "docs/notes"). */
  name: string;
  /** Full folder path as served (never empty for a child). */
  path: string;
  /** Descendant-inclusive document count from the server tree. */
  count: number;
};

function leafName(path: string): string {
  const i = path.lastIndexOf("/");
  return i >= 0 ? path.slice(i + 1) : path;
}

/** Walk to the node whose path equals `cwd`. Root `""` is the tree itself. */
function findNode(
  tree: FolderTreeNode[],
  cwd: string,
): FolderTreeNode | null {
  if (cwd === "") return null; // root is synthetic — children = tree
  const segs = cwd.split("/").filter(Boolean);
  let nodes = tree;
  let found: FolderTreeNode | null = null;
  let built = "";
  for (const seg of segs) {
    built = built ? `${built}/${seg}` : seg;
    found = nodes.find((n) => n.path === built) ?? null;
    if (!found) return null;
    nodes = found.children;
  }
  return found;
}

/**
 * Immediate child folders of `cwd`.
 *
 * - `cwd === ""` → top-level tree nodes
 * - nested cwd → that node's `.children`
 * - missing cwd → `[]` (never throws)
 *
 * `count` is the server's descendant-inclusive value, unchanged.
 * Result is sorted by `name` (localeCompare) for stable UI order.
 */
export function childFolders(
  tree: FolderTreeNode[],
  cwd: string,
): ChildFolder[] {
  const children =
    cwd === "" ? tree : (findNode(tree, cwd)?.children ?? []);
  return children
    .map((n) => ({
      name: leafName(n.path),
      path: n.path,
      count: n.count,
    }))
    .sort((a, b) => a.name.localeCompare(b.name));
}
