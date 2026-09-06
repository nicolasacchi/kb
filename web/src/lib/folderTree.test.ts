import { describe, it, expect } from "vitest";
import { childFolders, type FolderTreeNode } from "./folderTree";

const tree: FolderTreeNode[] = [
  {
    path: "docs",
    count: 10,
    children: [
      {
        path: "docs/notes",
        count: 4,
        children: [
          { path: "docs/notes/archive", count: 1, children: [] },
        ],
      },
      { path: "docs/research", count: 3, children: [] },
    ],
  },
  { path: "inbox", count: 2, children: [] },
];

describe("childFolders", () => {
  it("root cwd \"\" returns top-level folders with counts", () => {
    const kids = childFolders(tree, "");
    expect(kids).toEqual([
      { name: "docs", path: "docs", count: 10 },
      { name: "inbox", path: "inbox", count: 2 },
    ]);
  });

  it("nested cwd returns immediate children only", () => {
    const kids = childFolders(tree, "docs");
    expect(kids).toEqual([
      { name: "notes", path: "docs/notes", count: 4 },
      { name: "research", path: "docs/research", count: 3 },
    ]);
  });

  it("deeper nested cwd returns that node's children", () => {
    const kids = childFolders(tree, "docs/notes");
    expect(kids).toEqual([
      { name: "archive", path: "docs/notes/archive", count: 1 },
    ]);
  });

  it("leaf cwd returns empty children", () => {
    expect(childFolders(tree, "inbox")).toEqual([]);
    expect(childFolders(tree, "docs/research")).toEqual([]);
  });

  it("missing cwd returns [] (never throws)", () => {
    expect(childFolders(tree, "nope")).toEqual([]);
    expect(childFolders(tree, "docs/missing")).toEqual([]);
    expect(childFolders([], "docs")).toEqual([]);
  });

  it("count is descendant-inclusive as served (passthrough)", () => {
    // docs.count = 10 includes notes + research descendants — not recomputed
    const root = childFolders(tree, "");
    expect(root.find((c) => c.path === "docs")?.count).toBe(10);
    const underDocs = childFolders(tree, "docs");
    expect(underDocs.find((c) => c.path === "docs/notes")?.count).toBe(4);
  });

  it("sorts children by name", () => {
    const unsorted: FolderTreeNode[] = [
      { path: "zeta", count: 1, children: [] },
      { path: "alpha", count: 2, children: [] },
      { path: "mid", count: 3, children: [] },
    ];
    expect(childFolders(unsorted, "").map((c) => c.name)).toEqual([
      "alpha",
      "mid",
      "zeta",
    ]);
  });
});
