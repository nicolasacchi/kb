import { describe, expect, it } from "vitest";
import { folderIndexNote, folderIndexNoteRel } from "./folderIndexNote";

describe("folderIndexNoteRel", () => {
  it("joins nested folder + index.md", () => {
    expect(folderIndexNoteRel("changelog/daily")).toBe(
      "changelog/daily/index.md",
    );
    expect(folderIndexNoteRel("a")).toBe("a/index.md");
  });
  it("root folder is bare index.md", () => {
    expect(folderIndexNoteRel("")).toBe("index.md");
  });
});

describe("folderIndexNote", () => {
  const rows = [
    {
      source_relative: "a/b/note.md",
      title: "Note",
      summary: "x",
      id: "111",
    },
    {
      source_relative: "a/b/index.md",
      title: "Folder index",
      summary: "the folder note",
      id: "222",
    },
    {
      source_relative: "a/b/index.html",
      title: "HTML index",
      summary: null,
      id: "333",
    },
    {
      source_relative: "index.md",
      title: "Root note",
      summary: "root",
      id: "444",
    },
    {
      source_relative: "a/b/c/index.md",
      title: "Nested deeper",
      id: "555",
    },
  ];

  it("returns the exact <folder>/index.md row", () => {
    const hit = folderIndexNote(rows, "a/b");
    expect(hit?.id).toBe("222");
    expect(hit?.title).toBe("Folder index");
    expect(hit?.summary).toBe("the folder note");
  });

  it("does not match index.html or deeper descendants", () => {
    expect(folderIndexNote(rows, "a/b")?.source_relative).toBe("a/b/index.md");
    expect(folderIndexNote(rows, "a") ).toBeNull();
  });

  it("finds root index.md when folder is empty string", () => {
    expect(folderIndexNote(rows, "")?.id).toBe("444");
  });

  it("returns null when folder scope is absent (null/undefined)", () => {
    expect(folderIndexNote(rows, null)).toBeNull();
    expect(folderIndexNote(rows, undefined)).toBeNull();
  });

  it("returns null when no matching row", () => {
    expect(folderIndexNote(rows, "missing")).toBeNull();
    expect(folderIndexNote([], "a/b")).toBeNull();
  });
});
