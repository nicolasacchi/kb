import { describe, it, expect } from "vitest";
import {
  isUnchangedTarget,
  joinMoveTarget,
  validateFilename,
  validateFolderRenameTarget,
  validateRelPath,
} from "./movePath";

describe("validateRelPath", () => {
  it("accepts a clean multi-segment path", () => {
    expect(validateRelPath("docs/notes")).toEqual({
      ok: true,
      path: "docs/notes",
    });
  });

  it("accepts empty when allowEmpty", () => {
    expect(validateRelPath("", { allowEmpty: true })).toEqual({
      ok: true,
      path: "",
    });
  });

  it("rejects empty by default", () => {
    expect(validateRelPath("").ok).toBe(false);
  });

  it("rejects leading and trailing slashes", () => {
    expect(validateRelPath("/docs").ok).toBe(false);
    expect(validateRelPath("docs/").ok).toBe(false);
    expect(validateRelPath("/docs/").ok).toBe(false);
  });

  it("rejects empty segments / double slashes", () => {
    expect(validateRelPath("docs//notes").ok).toBe(false);
    expect(validateRelPath("a//b//c").ok).toBe(false);
  });

  it('rejects "." and ".." segments', () => {
    expect(validateRelPath("..").ok).toBe(false);
    expect(validateRelPath("docs/..").ok).toBe(false);
    expect(validateRelPath("docs/../x").ok).toBe(false);
    expect(validateRelPath("docs/.").ok).toBe(false);
  });
});

describe("validateFilename", () => {
  it("accepts a normal filename", () => {
    expect(validateFilename("readme.html")).toEqual({
      ok: true,
      path: "readme.html",
    });
  });

  it("rejects empty, slashes, and ..", () => {
    expect(validateFilename("").ok).toBe(false);
    expect(validateFilename("a/b.html").ok).toBe(false);
    expect(validateFilename("..").ok).toBe(false);
    expect(validateFilename(".").ok).toBe(false);
  });

  it("rejects leading/trailing space", () => {
    expect(validateFilename(" foo.html").ok).toBe(false);
    expect(validateFilename("foo.html ").ok).toBe(false);
  });
});

describe("joinMoveTarget", () => {
  it("joins folder + filename at root", () => {
    expect(
      joinMoveTarget({ folder: "", filename: "a.html" }),
    ).toEqual({ ok: true, path: "a.html" });
  });

  it("joins folder + filename", () => {
    expect(
      joinMoveTarget({ folder: "docs", filename: "a.html" }),
    ).toEqual({ ok: true, path: "docs/a.html" });
  });

  it("appends new subfolder under the picked folder", () => {
    expect(
      joinMoveTarget({
        folder: "docs",
        newSubfolder: "archive",
        filename: "a.html",
      }),
    ).toEqual({ ok: true, path: "docs/archive/a.html" });
  });

  it("appends multi-segment new subfolder", () => {
    expect(
      joinMoveTarget({
        folder: "docs",
        newSubfolder: "a/b",
        filename: "x.md",
      }),
    ).toEqual({ ok: true, path: "docs/a/b/x.md" });
  });

  it("new subfolder alone under root", () => {
    expect(
      joinMoveTarget({
        folder: "",
        newSubfolder: "inbox",
        filename: "x.md",
      }),
    ).toEqual({ ok: true, path: "inbox/x.md" });
  });

  it("rejects bad folder / subfolder / filename", () => {
    expect(
      joinMoveTarget({ folder: "/docs", filename: "a.html" }).ok,
    ).toBe(false);
    expect(
      joinMoveTarget({
        folder: "docs",
        newSubfolder: "../evil",
        filename: "a.html",
      }).ok,
    ).toBe(false);
    expect(
      joinMoveTarget({
        folder: "docs",
        newSubfolder: "a//b",
        filename: "a.html",
      }).ok,
    ).toBe(false);
    expect(
      joinMoveTarget({ folder: "docs", filename: "" }).ok,
    ).toBe(false);
    expect(
      joinMoveTarget({ folder: "docs", filename: "x/y.html" }).ok,
    ).toBe(false);
  });

  it("trims whitespace-only newSubfolder to absent", () => {
    expect(
      joinMoveTarget({
        folder: "docs",
        newSubfolder: "   ",
        filename: "a.html",
      }),
    ).toEqual({ ok: true, path: "docs/a.html" });
  });
});

describe("isUnchangedTarget", () => {
  it("detects same path", () => {
    expect(isUnchangedTarget("docs/a.html", "docs/a.html")).toBe(true);
    expect(isUnchangedTarget("docs/a.html", "docs/b.html")).toBe(false);
  });
});

describe("validateFolderRenameTarget", () => {
  it("accepts a clean folder path", () => {
    expect(validateFolderRenameTarget("docs/notes")).toEqual({
      ok: true,
      path: "docs/notes",
    });
  });

  it("rejects empty, slashes, and ..", () => {
    expect(validateFolderRenameTarget("").ok).toBe(false);
    expect(validateFolderRenameTarget("/docs").ok).toBe(false);
    expect(validateFolderRenameTarget("docs/").ok).toBe(false);
    expect(validateFolderRenameTarget("docs//x").ok).toBe(false);
    expect(validateFolderRenameTarget("docs/..").ok).toBe(false);
  });
});
