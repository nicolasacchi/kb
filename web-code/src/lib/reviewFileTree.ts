// V76-R2b — the review Files tab and the diff map column share ONE tree.
//
// Grouping is SECTIONS-OF-TREES, not a single mixed folder tree with
// status badges. The operator's 38-file screenshot already clustered into
// three status groups (including a "deleted 15" census); mixing those
// into one folder walk would scatter the deletes through the same
// directories as the adds and hide the count the screenshot was using.
// Each status is a collapsible section containing its own folder tree, so
// "15 deleted" stays a foldable census and folders still group the rest.
//
// Pure: files in, nodes out. The component renders rows it did not
// compute (kbc-tree/1).

import type { ReviewFileRow, SyntaxRowOut, TreeEntry } from "../api/types";
import { langIdForPath } from "./codeUrl";

export type FileStatusKind = "added" | "modified" | "renamed" | "deleted";

export const STATUS_SECTION_ORDER: readonly FileStatusKind[] = [
  "added",
  "modified",
  "renamed",
  "deleted",
];

export const STATUS_SECTION_LABEL: Record<FileStatusKind, string> = {
  added: "added",
  modified: "modified",
  renamed: "renamed",
  deleted: "deleted",
};

/// Git's own letter (and the longer words some older rows still send).
export function statusKind(status: string): FileStatusKind {
  const s = status.trim().toUpperCase();
  if (s.startsWith("A") || s === "ADDED") return "added";
  if (s.startsWith("D") || s === "DELETED") return "deleted";
  if (s.startsWith("R") || s === "RENAMED" || s.startsWith("C") || s === "COPIED") return "renamed";
  return "modified";
}

export interface FolderCounts {
  files: number;
  additions: number;
  deletions: number;
}

export interface FileTreeFileNode {
  kind: "file";
  path: string;
  name: string;
  file: ReviewFileRow;
}

export interface FileTreeFolderNode {
  kind: "folder";
  /// Directory path with no trailing slash (`app/models`). Root files
  /// live in a folder whose `path` is `""`.
  path: string;
  name: string;
  children: FileTreeNode[];
  counts: FolderCounts;
}

export type FileTreeNode = FileTreeFileNode | FileTreeFolderNode;

export interface StatusSection {
  status: FileStatusKind;
  label: string;
  files: ReviewFileRow[];
  counts: FolderCounts;
  tree: FileTreeNode[];
}

function emptyCounts(): FolderCounts {
  return { files: 0, additions: 0, deletions: 0 };
}

function addCounts(a: FolderCounts, b: FolderCounts): FolderCounts {
  return {
    files: a.files + b.files,
    additions: a.additions + b.additions,
    deletions: a.deletions + b.deletions,
  };
}

function fileCounts(f: ReviewFileRow): FolderCounts {
  return { files: 1, additions: f.additions, deletions: f.deletions };
}

function sortNodes(nodes: FileTreeNode[]): FileTreeNode[] {
  return [...nodes].sort((a, b) => {
    if (a.kind !== b.kind) return a.kind === "folder" ? -1 : 1;
    return a.name.localeCompare(b.name);
  });
}

/// Build a folder tree from `files`. Sibling order is folders then files,
/// each localeCompare'd — never the review's reading order, because a
/// folder tree is a PATH projection, not a walk of the patch.
export function buildFolderTree(files: readonly ReviewFileRow[]): FileTreeNode[] {
  interface MutableFolder {
    path: string;
    name: string;
    folders: Map<string, MutableFolder>;
    files: FileTreeFileNode[];
  }
  const root: MutableFolder = { path: "", name: "", folders: new Map(), files: [] };

  function ensure(dirPath: string): MutableFolder {
    if (dirPath === "") return root;
    const parts = dirPath.split("/").filter((p) => p.length > 0);
    let cur = root;
    let acc = "";
    for (const part of parts) {
      acc = acc === "" ? part : `${acc}/${part}`;
      let next = cur.folders.get(part);
      if (!next) {
        next = { path: acc, name: part, folders: new Map(), files: [] };
        cur.folders.set(part, next);
      }
      cur = next;
    }
    return cur;
  }

  for (const f of files) {
    const slash = f.path.lastIndexOf("/");
    const dir = slash === -1 ? "" : f.path.slice(0, slash);
    const name = slash === -1 ? f.path : f.path.slice(slash + 1);
    const parent = ensure(dir);
    parent.files.push({ kind: "file", path: f.path, name, file: f });
  }

  function freeze(folder: MutableFolder): FileTreeNode[] {
    const children: FileTreeNode[] = [];
    for (const sub of folder.folders.values()) {
      const kids = freeze(sub);
      const counts = kids.reduce(
        (acc, n) => addCounts(acc, n.kind === "folder" ? n.counts : fileCounts(n.file)),
        emptyCounts(),
      );
      children.push({
        kind: "folder",
        path: sub.path,
        name: sub.name,
        children: kids,
        counts,
      });
    }
    children.push(...folder.files);
    return sortNodes(children);
  }

  return freeze(root);
}

export function sectionCounts(files: readonly ReviewFileRow[]): FolderCounts {
  return files.reduce((acc, f) => addCounts(acc, fileCounts(f)), emptyCounts());
}

/// Group `files` into status sections, each with its own folder tree.
/// Empty statuses are omitted (a count of zero is not rendered).
/// File order inside a section follows the caller's order into the tree
/// builder, which then sorts by path — the section membership is the
/// status census; the tree is the path census.
export function buildStatusSections(files: readonly ReviewFileRow[]): StatusSection[] {
  const buckets = new Map<FileStatusKind, ReviewFileRow[]>();
  for (const k of STATUS_SECTION_ORDER) buckets.set(k, []);
  for (const f of files) {
    const k = statusKind(f.status);
    buckets.get(k)!.push(f);
  }
  const out: StatusSection[] = [];
  for (const status of STATUS_SECTION_ORDER) {
    const list = buckets.get(status)!;
    if (list.length === 0) continue;
    out.push({
      status,
      label: STATUS_SECTION_LABEL[status],
      files: list,
      counts: sectionCounts(list),
      tree: buildFolderTree(list),
    });
  }
  return out;
}

/// Middle-truncate a path so the FIRST character is always from the path
/// (never a leading ellipsis) and the basename stays visible. `maxChars`
/// is a character budget, not pixels — the row still sets `title` to the
/// full path.
export function middleTruncate(path: string, maxChars: number): string {
  if (maxChars <= 0) return "";
  if (path.length <= maxChars) return path;
  if (maxChars === 1) return path.slice(0, 1);
  if (maxChars === 2) return path.slice(0, 1) + "…";

  const slash = path.lastIndexOf("/");
  const base = slash >= 0 ? path.slice(slash + 1) : path;

  // Basename longer than the budget: keep the start of the name AND the
  // end, so the result never begins with `…`.
  if (base.length >= maxChars - 1 || slash < 0) {
    const keepHead = Math.max(1, Math.ceil((maxChars - 1) / 2));
    const keepTail = maxChars - 1 - keepHead;
    if (keepTail <= 0) return path.slice(0, maxChars - 1) + "…";
    return path.slice(0, keepHead) + "…" + path.slice(path.length - keepTail);
  }

  // Keep the full basename (with its leading slash) and as much of the
  // directory prefix as the leftover budget allows.
  const tail = path.slice(slash); // "/basename"
  const budget = maxChars - 1 - tail.length; // 1 for the ellipsis
  const prefix = path.slice(0, Math.max(1, budget));
  return prefix + "…" + tail;
}

export function countsText(c: FolderCounts): string {
  const n = c.files === 1 ? "1 file" : `${c.files} files`;
  return `${n} +${c.additions} −${c.deletions}`;
}

/// Lang id for a path against a `syntax/1` registry payload. `rows` may
/// be missing (the fetch has not landed, or an older daemon) — then we
/// fall back to `langIdForPath`'s hand-mirrored table rather than invent
/// a language. Filenames beat extensions (Gemfile has no extension).
export function langIdFromSyntax(
  path: string,
  rows: readonly SyntaxRowOut[] | null | undefined,
): string | null {
  const base = path.split("/").filter((p) => p.length > 0).pop() ?? path;
  const dot = base.lastIndexOf(".");
  const ext = dot > 0 ? base.slice(dot + 1).toLowerCase() : "";
  if (rows && rows.length > 0) {
    for (const row of rows) {
      if (row.filenames?.some((f) => f === base)) return row.lang;
    }
    if (ext) {
      for (const row of rows) {
        if (row.extensions?.some((e) => e.toLowerCase() === ext)) return row.lang;
      }
    }
  }
  return langIdForPath(path);
}

export interface FlatTreeRow {
  key: string;
  depth: number;
  node: FileTreeNode;
}

export interface VisibleTreeRow extends FlatTreeRow {
  status: FileStatusKind;
}

/// Flatten ONE tree for keyboard walking, honouring which folders are
/// collapsed. A collapsed folder hides its descendants. No notion of a
/// status section — that's `flattenVisible`'s own layer, built on top of
/// this (V80-M1: the "All files" tree has no sections at all, and calls
/// this directly).
export function flattenTree(
  tree: readonly FileTreeNode[],
  collapsedFolders: ReadonlySet<string>,
): FlatTreeRow[] {
  const out: FlatTreeRow[] = [];
  function walk(nodes: readonly FileTreeNode[], depth: number) {
    for (const node of nodes) {
      out.push({ key: node.kind === "folder" ? `dir:${node.path}` : node.path, depth, node });
      if (node.kind === "folder" && !collapsedFolders.has(node.path)) {
        walk(node.children, depth + 1);
      }
    }
  }
  walk(tree, 0);
  return out;
}

/// Flatten the sections for keyboard walking, honouring which folders
/// and which status sections are collapsed. A collapsed folder hides its
/// descendants; a collapsed section hides its tree but keeps the header
/// (the header is not in this list — the component renders it).
export function flattenVisible(
  sections: readonly StatusSection[],
  collapsedFolders: ReadonlySet<string>,
  collapsedSections: ReadonlySet<FileStatusKind>,
): VisibleTreeRow[] {
  const out: VisibleTreeRow[] = [];
  for (const sec of sections) {
    if (collapsedSections.has(sec.status)) continue;
    for (const row of flattenTree(sec.tree, collapsedFolders)) {
      out.push({ ...row, status: sec.status });
    }
  }
  return out;
}

// --- V80-M1 — "All files": one flat tree over the union of the changed
// rows and the tip sha's whole tree, changed files marked, everything else
// plain (`web-code/CLAUDE.md`'s Review diff v2 section). ------------------

/// Build the union tree: every `changed` row keeps its REAL `ReviewFileRow`
/// (real status, real +/- counts); every OTHER path in `allPaths` is
/// synthesized with `status: ""` — deliberately outside `FileStatusKind`'s
/// closed vocabulary (`statusKind` is never called on it), so a renderer
/// can tell "plain" from "changed" with a single `changedPaths.has(path)`
/// check rather than by inspecting a fabricated status letter.
export function buildAllFilesTree(
  changed: readonly ReviewFileRow[],
  allPaths: readonly string[],
): { tree: FileTreeNode[]; changedPaths: ReadonlySet<string> } {
  const changedPaths = new Set(changed.map((f) => f.path));
  const rows: ReviewFileRow[] = [...changed];
  for (const p of allPaths) {
    if (changedPaths.has(p)) continue;
    rows.push({
      path: p,
      old_path: null,
      status: "",
      additions: 0,
      deletions: 0,
      blob_sha: "",
      viewed: false,
      viewed_stale: false,
      open_annotations: 0,
    });
  }
  return { tree: buildFolderTree(rows), changedPaths };
}

/// How many leaf paths one "All files" walk will ever hold. Bounds memory
/// and render cost on a very large repo; `AllFilesResult.capped` says so on
/// the wire rather than truncating silently.
export const ALL_FILES_CAP = 4000;

export interface AllFilesResult {
  paths: string[];
  capped: boolean;
}

interface AllFilesSink {
  paths: string[];
  capped: boolean;
}

async function walkAllFilesDir(
  listDir: (dir: string) => Promise<readonly TreeEntry[]>,
  dir: string,
  cap: number,
  sink: AllFilesSink,
): Promise<void> {
  if (sink.capped) return;
  const entries = await listDir(dir);
  const dirs: string[] = [];
  for (const e of entries) {
    if (sink.capped) break;
    const p = dir ? `${dir}/${e.name}` : e.name;
    if (e.kind === "dir") {
      dirs.push(p);
      continue;
    }
    // "file" / "symlink" / "submodule" are all LEAVES here — a submodule
    // is reported, never descended into (mirrors `git::tree::EntryKind`'s
    // own doc server-side: it has no tree of its own this handle can
    // walk), but it is still part of "the tip sha's full tree" and stays
    // in the list rather than vanishing silently.
    if (sink.paths.length >= cap) {
      sink.capped = true;
      break;
    }
    sink.paths.push(p);
  }
  if (sink.capped || dirs.length === 0) return;
  // Sibling directories fan out concurrently (the same shape server-side
  // `buffered_join` uses, #28) — a deep tree should not pay for its depth
  // in serial round trips. `sink` is shared and mutated in place; safe
  // under JS's single-threaded concurrency, and `sink.capped` short-
  // circuits every in-flight sibling on the next entry it checks.
  await Promise.all(dirs.map((d) => walkAllFilesDir(listDir, d, cap, sink)));
}

/// Recursively walk a per-directory tree listing (`listDir`, injected so
/// this stays pure/testable without a network mock — `hooks/
/// useReviewAllFiles.ts` wires it to `GET /api/tree`) into a flat, sorted
/// leaf-path list, capped at `cap`. `listDir("")` is the root.
export async function walkAllFiles(
  listDir: (dir: string) => Promise<readonly TreeEntry[]>,
  cap: number = ALL_FILES_CAP,
): Promise<AllFilesResult> {
  const sink: AllFilesSink = { paths: [], capped: false };
  await walkAllFilesDir(listDir, "", cap, sink);
  sink.paths.sort();
  return { paths: sink.paths, capped: sink.capped };
}
