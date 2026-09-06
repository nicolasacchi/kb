import { describe, expect, it } from "vitest";
import type { TreeRow } from "../api/types";
import {
  ancestorDirs,
  dirKey,
  expandParam,
  nextRowWith,
  routeFilterBox,
  selectionCli,
  stickyAncestors,
  treeCli,
  SELECTION_ACTIONS,
} from "./treeQuery";
import { legacyRowsToTreeRows } from "./tree";
import { KBC_COMMANDS } from "../commands/registry.gen";

function row(p: Partial<TreeRow> & { id: string; depth: number }): TreeRow {
  return {
    kind: "file",
    label: p.id,
    children: 0,
    files: 1,
    ...p,
  } as TreeRow;
}

describe("routeFilterBox — one box, two destinations", () => {
  it("sends plain text to the fuzzy filter", () => {
    expect(routeFilterBox("order")).toEqual({ filter: "order" });
    expect(routeFilterBox("  order spec ")).toEqual({ filter: "order spec" });
  });

  it("sends anything structured to the scope param", () => {
    expect(routeFilterBox("role:spec")).toEqual({ scope: "role:spec" });
    expect(routeFilterBox("$generated")).toEqual({ scope: "$generated" });
    expect(routeFilterBox("role:model && !path:spec//*")).toEqual({
      scope: "role:model && !path:spec//*",
    });
    expect(routeFilterBox("a || b")).toEqual({ scope: "a || b" });
    expect(routeFilterBox("-ext:min.js")).toEqual({ scope: "-ext:min.js" });
  });

  it("is empty for an empty box (no scope, no filter)", () => {
    expect(routeFilterBox("")).toEqual({});
    expect(routeFilterBox("   ")).toEqual({});
  });

  it("does not treat a bare colon-free word with a dash as structured", () => {
    // A filename fragment, not a negation.
    expect(routeFilterBox("some-file")).toEqual({ filter: "some-file" });
  });
});

describe("stickyAncestors — Zed's sticky scroll for a tree", () => {
  const rows: TreeRow[] = [
    row({ id: "app", depth: 0, kind: "dir" }),
    row({ id: "app/models", depth: 1, kind: "dir" }),
    row({ id: "app/models/a.rb", depth: 2 }),
    row({ id: "app/models/b.rb", depth: 2 }),
    row({ id: "app/x.rb", depth: 1 }),
  ];

  it("pins each strictly-shallower ancestor, deepest last", () => {
    expect(stickyAncestors(rows, 3).map((r) => r.id)).toEqual(["app", "app/models"]);
  });

  it("pins nothing above a depth-0 row, and nothing at the top", () => {
    expect(stickyAncestors(rows, 0)).toEqual([]);
    expect(stickyAncestors(rows, 4).map((r) => r.id)).toEqual(["app"]);
  });

  it("is total on an out-of-range index", () => {
    expect(stickyAncestors(rows, 99)).toEqual([]);
    expect(stickyAncestors([], 0)).toEqual([]);
  });
});

describe("nextRowWith — the ]c / ]a traversal", () => {
  const rows: TreeRow[] = [
    row({ id: "d", depth: 0, kind: "dir", facts: { git_changed: 2 } }),
    row({ id: "a.rb", depth: 1, facts: { git: "M" } }),
    row({ id: "b.rb", depth: 1, facts: { annot: 2 } }),
    row({ id: "c.rb", depth: 1, facts: { git: "A", annot: 1 } }),
  ];

  it("walks forwards and backwards over FILE rows only", () => {
    expect(nextRowWith(rows, 0, 1, "change")).toBe(1);
    expect(nextRowWith(rows, 1, 1, "change")).toBe(3);
    expect(nextRowWith(rows, 3, -1, "change")).toBe(1);
    expect(nextRowWith(rows, 0, 1, "annot")).toBe(2);
  });

  it("returns -1 rather than wrapping onto a row already seen", () => {
    expect(nextRowWith(rows, 3, 1, "change")).toBe(-1);
    expect(nextRowWith(rows, 1, -1, "change")).toBe(-1);
    expect(nextRowWith([], 0, 1, "annot")).toBe(-1);
  });

  it("ignores a folder's aggregate — a folder is not a changed FILE", () => {
    expect(nextRowWith(rows, -1, 1, "change")).toBe(1);
  });
});

describe("expandParam / dirKey / ancestorDirs", () => {
  it("dedupes and drops empties", () => {
    expect(expandParam(["d:a", "d:a", "", "d:b"])).toBe("d:a,d:b");
    expect(expandParam([])).toBe("");
  });

  it("keys a directory the way the daemon does", () => {
    expect(dirKey("app/models")).toBe("d:app/models");
  });

  it("walks a path's ancestors, deepest last, leaf excluded", () => {
    expect(ancestorDirs("a/b/c.rs")).toEqual(["a", "a/b"]);
    expect(ancestorDirs("top.rs")).toEqual([]);
  });
});

describe("the CLI is printed, and it is honest about what it can reproduce", () => {
  it("renders the tree's own command from the live view state", () => {
    expect(
      treeCli({ repo: "kb", view: "role", scope: "role:spec", mode: "highlight", decorate: ["git"] }),
    ).toBe(
      "kb-code tree --repo kb --daemon http://127.0.0.1:4747 --view role --scope role:spec --mode highlight --decorate git",
    );
  });

  it("quotes anything a shell would eat", () => {
    const cli = treeCli({ repo: "kb", view: "physical", scope: "role:model && !path:spec//*" });
    expect(cli).toContain("'role:model && !path:spec//*'");
  });

  it("gives every selection action a command", () => {
    for (const a of SELECTION_ACTIONS) {
      const cli = selectionCli(a.id, { repo: "kb", paths: ["app/a.rb", "app/b.rb"] });
      expect(cli.startsWith("kb-code ")).toBe(true);
      // An action whose command is a TEMPLATE must say so on the row, and
      // an action claiming `exact` must not carry a trailing `#` comment.
      if (a.exact) expect(cli.includes("#")).toBe(false);
      else expect(a.note && a.note.length > 10).toBe(true);
    }
  });

  it("the scope action prints exactly the verb that exists", () => {
    expect(selectionCli("scope", { repo: "kb", paths: ["a.rb"] })).toBe(
      "kb-code scope from-paths a.rb --repo kb --daemon http://127.0.0.1:4747",
    );
  });
});

describe("legacyRowsToTreeRows — the ref-browsing adapter", () => {
  it("converts the per-directory listing into the same row shape", () => {
    const legacy = [
      { path: "app", name: "app", kind: "dir" as const, depth: 0, size: null, oid: "" },
      { path: "app/a.rb", name: "a.rb", kind: "file" as const, depth: 1, size: 1, oid: "x" },
    ];
    const rows = legacyRowsToTreeRows(legacy, new Set(["app"]));
    expect(rows.map((r) => r.id)).toEqual(["d:app", "f:app/a.rb"]);
    expect(rows[0].files).toBe(1);
    expect(rows[0].has_more).toBe(false);
    // No decoration lanes at a ref — absent, never zeroed.
    expect(rows[0].facts).toBeUndefined();
    expect(rows[1].trust).toBeUndefined();
  });

  it("reports has_more for a directory whose listing has not arrived", () => {
    const legacy = [
      { path: "app", name: "app", kind: "dir" as const, depth: 0, size: null, oid: "" },
    ];
    expect(legacyRowsToTreeRows(legacy, new Set())[0].has_more).toBe(true);
  });
});

describe("V71-F1's registry rows are declared where the SPA can dispatch them", () => {
  const ids = [
    "tree.view.next",
    "tree.view.prev",
    "tree.filter.mode",
    "tree.decorations.cycle",
    "tree.select.toggle",
    "tree.actions",
    "tree.next-change",
    "tree.prev-change",
    "tree.next-annot",
    "tree.prev-annot",
    "tree.reveal",
  ];

  it("every new row exists in the generated registry", () => {
    for (const id of ids) {
      expect(KBC_COMMANDS.find((c) => c.id === id), id).toBeTruthy();
    }
  });

  it("the tree rows live in the tree scope, and reveal is global", () => {
    for (const id of ids.filter((i) => i !== "tree.reveal")) {
      expect(KBC_COMMANDS.find((c) => c.id === id)?.scope, id).toBe("tree");
    }
    expect(KBC_COMMANDS.find((c) => c.id === "tree.reveal")?.scope).toBe("global");
  });

  it("reveal is NOT bound to `g r` — the buffer already owns that prefix", () => {
    const reveal = KBC_COMMANDS.find((c) => c.id === "tree.reveal");
    const keys = [...reveal!.keys.vim, ...reveal!.keys.plain, ...reveal!.keys.helix];
    expect(keys.some((k) => k === "g r" || k.startsWith("g "))).toBe(false);
    expect(reveal!.keys.vim).toContain("Space g f");
  });

  it("no new tree row carries a vim_kind (none of them lives in the buffer)", () => {
    for (const id of ids.filter((i) => i !== "tree.reveal")) {
      expect(KBC_COMMANDS.find((c) => c.id === id)?.vimKind ?? null, id).toBeNull();
    }
  });
});
