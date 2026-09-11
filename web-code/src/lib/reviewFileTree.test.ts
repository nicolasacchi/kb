import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { ReviewFileRow, SyntaxRowOut } from "../api/types";
import { mergeQuery, reviewDiffHref } from "./codeUrl";
import {
  buildFolderTree,
  buildStatusSections,
  countsText,
  flattenVisible,
  langIdFromSyntax,
  middleTruncate,
  statusKind,
  type FileTreeFolderNode,
} from "./reviewFileTree";

function file(path: string, over: Partial<ReviewFileRow> = {}): ReviewFileRow {
  return {
    path,
    old_path: null,
    status: "M",
    additions: 1,
    deletions: 0,
    blob_sha: "a".repeat(40),
    viewed: false,
    viewed_stale: false,
    open_annotations: 0,
    ...over,
  };
}

describe("statusKind", () => {
  it("maps git letters and words", () => {
    expect(statusKind("A")).toBe("added");
    expect(statusKind("added")).toBe("added");
    expect(statusKind("D")).toBe("deleted");
    expect(statusKind("R100")).toBe("renamed");
    expect(statusKind("C")).toBe("renamed");
    expect(statusKind("M")).toBe("modified");
    expect(statusKind("?")).toBe("modified");
  });
});

describe("buildFolderTree", () => {
  it("nests by directory and counts files and ± lines", () => {
    const tree = buildFolderTree([
      file("app/models/foo.rb", { status: "A", additions: 10, deletions: 0 }),
      file("app/models/bar.rb", { status: "M", additions: 2, deletions: 1 }),
      file("lib/tasks/x.rake", { status: "A", additions: 4, deletions: 0 }),
    ]);
    expect(tree.map((n) => n.name)).toEqual(["app", "lib"]);
    const app = tree[0] as FileTreeFolderNode;
    expect(app.kind).toBe("folder");
    expect(app.counts).toEqual({ files: 2, additions: 12, deletions: 1 });
    expect(app.children).toHaveLength(1);
    const models = app.children[0] as FileTreeFolderNode;
    expect(models.name).toBe("models");
    expect(models.children.map((n) => n.name)).toEqual(["bar.rb", "foo.rb"]);
  });

  it("keeps root files at the top level, after folders", () => {
    const tree = buildFolderTree([file("z.rs"), file("a/b.rs")]);
    expect(tree.map((n) => `${n.kind}:${n.name}`)).toEqual(["folder:a", "file:z.rs"]);
  });
});

describe("buildStatusSections", () => {
  it("emits sections-of-trees in added/modified/renamed/deleted order and omits empty ones", () => {
    const sections = buildStatusSections([
      file("app/a.rb", { status: "M", additions: 3, deletions: 1 }),
      file("app/b.rb", { status: "A", additions: 8, deletions: 0 }),
      file("gone.rb", { status: "D", additions: 0, deletions: 5 }),
      file("app/c.rb", { status: "D", additions: 0, deletions: 2 }),
    ]);
    expect(sections.map((s) => [s.status, s.counts.files, s.counts.additions, s.counts.deletions])).toEqual([
      ["added", 1, 8, 0],
      ["modified", 1, 3, 1],
      ["deleted", 2, 0, 7],
    ]);
    const deleted = sections.find((s) => s.status === "deleted")!;
    expect(deleted.tree.map((n) => n.name).sort()).toEqual(["app", "gone.rb"]);
  });
});

describe("middleTruncate", () => {
  it("is a no-op when the path fits", () => {
    expect(middleTruncate("app/foo.rb", 40)).toBe("app/foo.rb");
  });

  it("never produces a leading ellipsis — keeps a prefix crumb and the basename", () => {
    const long = "app/models/pricing/competitors_minimum_prices.before_deploy.rb";
    const out = middleTruncate(long, 36);
    expect(out.startsWith("…")).toBe(false);
    expect(out.startsWith("app")).toBe(true);
    expect(out.includes("…")).toBe(true);
    expect(out.length).toBeLessThanOrEqual(36);
    // The operator screenshot's leading-ellipsis form must never appear.
    expect(out).not.toMatch(/^…/);
  });

  it("middle-truncates a too-long basename without a leading ellipsis", () => {
    const out = middleTruncate("verylongbasenamewithoutaslash.rb", 12);
    expect(out.startsWith("…")).toBe(false);
    expect(out.includes("…")).toBe(true);
    expect(out.length).toBeLessThanOrEqual(12);
  });
});

describe("langIdFromSyntax", () => {
  const rows: SyntaxRowOut[] = [
    { lang: "ruby", extensions: ["rb", "rake"], filenames: ["Gemfile"] },
    { lang: "rust", extensions: ["rs"] },
  ];
  it("matches filenames before extensions", () => {
    expect(langIdFromSyntax("Gemfile", rows)).toBe("ruby");
    expect(langIdFromSyntax("app/foo.rb", rows)).toBe("ruby");
    expect(langIdFromSyntax("lib.rs", rows)).toBe("rust");
  });
  it("falls back to langIdForPath when the registry has not landed", () => {
    expect(langIdFromSyntax("lib.rs", null)).toBe("rust");
    expect(langIdFromSyntax("lib.rs", [])).toBe("rust");
  });
  it("guards absent extensions/filenames arrays", () => {
    expect(langIdFromSyntax("x.rs", [{ lang: "rust" }])).toBe("rust");
  });
});

describe("flattenVisible", () => {
  it("hides descendants of a collapsed folder and a collapsed section", () => {
    const sections = buildStatusSections([
      file("app/models/a.rb", { status: "A" }),
      file("lib/b.rb", { status: "A" }),
      file("z.rs", { status: "M" }),
    ]);
    const all = flattenVisible(sections, new Set(), new Set());
    expect(all.some((r) => r.node.kind === "file" && r.node.path === "app/models/a.rb")).toBe(true);
    const collapsedApp = flattenVisible(sections, new Set(["app"]), new Set());
    expect(collapsedApp.some((r) => r.key === "dir:app")).toBe(true);
    expect(collapsedApp.some((r) => r.node.kind === "file" && r.node.path === "app/models/a.rb")).toBe(
      false,
    );
    const noAdded = flattenVisible(sections, new Set(), new Set(["added"]));
    expect(noAdded.every((r) => r.status === "modified")).toBe(true);
  });
});

describe("URL sync (Location Contract ?file=)", () => {
  it("a tree pick is the path-segment plus ?file=, with file last", () => {
    const href = reviewDiffHref("demo", 9, "app/models/foo.rb", { file: "app/models/foo.rb" });
    expect(href.startsWith("/r/demo/~reviews/9/diff/app/models/foo.rb")).toBe(true);
    expect(href).toContain("file=app%2Fmodels%2Ffoo.rb");
    expect(href.indexOf("/foo.rb")).toBeLessThan(href.indexOf("file="));
  });

  it("mergeQuery overlays ?file= onto a live query without dropping other params", () => {
    const href = reviewDiffHref("demo", 9, "app/a.rb");
    const withLive = `${href}?ps=2&view=split`;
    expect(mergeQuery(withLive, { file: "app/a.rb" })).toBe(
      "/r/demo/~reviews/9/diff/app/a.rb?ps=2&view=split&file=app%2Fa.rb",
    );
  });
});

describe("countsText", () => {
  it("pluralises files and always prints both signs", () => {
    expect(countsText({ files: 1, additions: 4, deletions: 0 })).toBe("1 file +4 −0");
    expect(countsText({ files: 15, additions: 0, deletions: 40 })).toBe("15 files +0 −40");
  });
});

describe("shipped e2e hooks the tree / map must keep", () => {
  it("ReviewFileTree still stamps data-kbc-rdiff-map-row and data-kbc-rdiff-map-current", () => {
    const src = readFileSync(fileURLToPath(new URL("../components/reviews/ReviewFileTree.tsx", import.meta.url)), "utf-8");
    expect(src).toContain('attrs["data-kbc-rdiff-map-row"] = path');
    expect(src).toContain('attrs["data-kbc-rdiff-map-current"] = "1"');
  });

  it("]f / goFile in all-files mode does not write ?file= (map click does)", () => {
    const src = readFileSync(fileURLToPath(new URL("../routes/ReviewDiff.tsx", import.meta.url)), "utf-8");
    const goFile = src.slice(src.indexOf("function goFile"), src.indexOf("function openFileInCenter"));
    expect(goFile).toContain('apply({ type: "gotoFile"');
    expect(goFile).not.toContain('setParam("file"');
    const open = src.slice(src.indexOf("function openFileInCenter"), src.indexOf("function openPseudo"));
    expect(open).toContain("file:");
  });

  it("first-hunk scroll after a tree click keys off focusPath, not all-files ?file=", () => {
    const src = readFileSync(fileURLToPath(new URL("../routes/ReviewDiff.tsx", import.meta.url)), "utf-8");
    const start = src.indexOf("const hunkScrolledFor");
    expect(start).toBeGreaterThan(0);
    const effect = src.slice(start, start + 1500);
    expect(effect).toContain("const path = focusPath;");
    expect(effect).not.toContain("focusPath || fileHint");
  });

  it("the stream is a document again (no nested height:100%/overflow clip)", () => {
    const css = readFileSync(fileURLToPath(new URL("../styles/reviews.css", import.meta.url)), "utf-8");
    const stream = css.match(/\.kbc-rdiff__stream \{[^}]+\}/)?.[0] ?? "";
    expect(stream).not.toContain("height: 100%");
    expect(stream).not.toContain("overflow: auto");
    expect(css).toContain("position: sticky");
  });

  it("sticky section-head does not use --z-bar or a mapped topbar offset (would cover the first strip)", () => {
    const css = readFileSync(fileURLToPath(new URL("../styles/reviews.css", import.meta.url)), "utf-8");
    const head = css.match(/\.kbc-rdiff__section-head \{[^}]+\}/)?.[0] ?? "";
    expect(head).toContain("position: sticky");
    expect(head).toMatch(/z-index:\s*1;/);
    expect(head).not.toContain("var(--z-bar)");
    const mapped = css.match(/\.kbc-rdiff__body--mapped \.kbc-rdiff__section-head \{[^}]+\}/)?.[0] ?? "";
    expect(mapped).toMatch(/top:\s*0;/);
    expect(mapped).not.toContain("var(--topbar-h)");
  });

  it("hunk strip stacks above the section-head and has scroll-margin below the sticky chrome", () => {
    const css = readFileSync(fileURLToPath(new URL("../styles/reviews.css", import.meta.url)), "utf-8");
    const head = css.match(/\.kbc-rdiff__section-head \{[^}]+\}/)?.[0] ?? "";
    const strip = css.match(/\.kbc-hunkstrip \{[^}]+\}/)?.[0] ?? "";
    const headZ = Number((head.match(/z-index:\s*(\d+)/) ?? [])[1]);
    const stripZ = Number((strip.match(/z-index:\s*(\d+)/) ?? [])[1]);
    expect(headZ).toBeGreaterThan(0);
    expect(stripZ).toBeGreaterThan(headZ);
    expect(strip).toContain("position: relative");
    expect(strip).toContain("scroll-margin-top:");
    expect(css).toMatch(/\.kbc-rdiff__section \{[^}]*isolation:\s*isolate;/);
  });

  it("both viewed controls stay on the header and the strip (operator ruling)", () => {
    const section = readFileSync(
      fileURLToPath(new URL("../routes/reviewDiff/DiffSections.tsx", import.meta.url)),
      "utf-8",
    );
    const center = readFileSync(
      fileURLToPath(new URL("../routes/reviewDiff/ReviewDiffCenter.tsx", import.meta.url)),
      "utf-8",
    );
    const strip = readFileSync(fileURLToPath(new URL("../components/diff/HunkStrip.tsx", import.meta.url)), "utf-8");
    expect(section).toContain("data-kbc-review-viewed={file.path}");
    expect(center).toContain("data-kbc-review-viewed={file.path}");
    expect(strip).toContain("data-kbc-hunk-viewed-toggle={view.id}");
    expect(section).toContain('className="kbc-rdiff__section-head"');
    expect(strip).toContain("kbc-hunkstrip__viewed");
  });

  it("ReviewMapSplit panels overflow:visible so the page, not the panel, scrolls", () => {
    const src = readFileSync(
      fileURLToPath(new URL("../components/reviews/ReviewMapSplit.tsx", import.meta.url)),
      "utf-8",
    );
    expect(src).toContain('style={{ overflow: "visible" }}');
    expect(src).not.toContain('overflow: "hidden"');
  });
});

describe("tree keydown scope (V76-R2b.3)", () => {
  const src = readFileSync(
    fileURLToPath(new URL("../components/reviews/ReviewFileTree.tsx", import.meta.url)),
    "utf8",
  );
  it("keys typed inside the expanded inline diff never drive tree navigation", () => {
    // The inline diff (composer, CM6 suggestion editor) is `expandedContent`
    // under the picked row, so its keystrokes bubble to the tree's
    // onKeyDown; an `Enter` there re-picked the file and remounted the
    // editor mid-edit. The guard bails unless the key came from the tree
    // root or a `.kbc-ftree__row`, and always for editable targets.
    const body = src.slice(src.indexOf("function onTreeKey("));
    const guard = body.indexOf('closest(".kbc-ftree__row")');
    const editable = body.indexOf("isContentEditable");
    const keys = body.indexOf('new Set(["j", "k"');
    expect(guard).toBeGreaterThan(-1);
    expect(editable).toBeGreaterThan(-1);
    expect(guard).toBeLessThan(keys);
    expect(editable).toBeLessThan(keys);
  });
});
