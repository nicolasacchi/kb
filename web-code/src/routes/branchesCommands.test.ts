import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { KBC_COMMANDS } from "../commands/registry.gen";
import { BRANCHES_COMMAND_IDS } from "./branchesCommands";

// V75-M3 — the declaration ↔ handler walk, in `recipeCommands.test.ts`'s
// own shape. `commands/branches.test.ts` proves the KEYS are safe;
// `commands/surfaceRows.test.ts` proves each row is claimed at all; this
// file proves the page's own id list and the registry's `scope: branches`
// rows are the SAME SET, both ways.

describe("~branches — declaration ↔ handler walk", () => {
  const declared = KBC_COMMANDS.filter((c) => c.scope === "branches");

  it("has rows (a scope with none is a dead scope)", () => {
    expect(declared.length).toBeGreaterThan(0);
  });

  /// Forwards: a row shipped in the registry that nothing on the page runs.
  it("every scope:branches registry row is owned by the page", () => {
    for (const c of declared) {
      expect(
        (BRANCHES_COMMAND_IDS as readonly string[]).includes(c.id),
        `${c.id} is in registry.json but nothing on ~branches handles it`,
      ).toBe(true);
    }
  });

  /// Backwards: a handler for a command that no longer exists.
  it("every id the page claims exists in the registry", () => {
    const ids = new Set(declared.map((c) => c.id));
    for (const id of BRANCHES_COMMAND_IDS) {
      expect(ids.has(id), `${id} has a handler but no registry row`).toBe(true);
    }
  });

  it("no id is declared twice", () => {
    expect(new Set(BRANCHES_COMMAND_IDS).size).toBe(BRANCHES_COMMAND_IDS.length);
  });

  /// The page owns a text FILTER, so a bare printable key is only safe
  /// because `BranchViews.onKeyDown` bails on a typing target first
  /// (`CommandRoot`'s own guard 1, this surface's copy). That bail is the
  /// thing this assertion protects: remove it and every bare row here
  /// becomes a key you cannot type into the filter.
  it("the surface guards its bare keys against its own filter box", () => {
    const src = new URL("../components/branches/BranchViews.tsx", import.meta.url);
    const body = readFileSync(fileURLToPath(src), "utf8");
    expect(body).toMatch(/isTypingTarget\(e\.target\)/);
    const bare = declared.flatMap((c) => c.keys.vim).filter((k) => /^[!-~]$/.test(k));
    expect(bare.length, "this family is bare-key-based; if that changed, revisit the guard").toBeGreaterThan(0);
  });
});
