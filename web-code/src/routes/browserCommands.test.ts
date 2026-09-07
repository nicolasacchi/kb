import { describe, expect, it } from "vitest";
import { KBC_ACTIVE_COMMANDS } from "../commands/registry.gen";
import { BROWSER_COMMAND_IDS } from "./browserCommands";

describe("symbol browser (~browser) — declaration ↔ handler walk", () => {
    // V74-L3b — a PREFIX match, not an exact one. The surface atom is still
    // the first atom of every row this page owns, but a row may now carry a
    // second (`&& !walkthrough`, added so `whenDisjoint` can prove it apart
    // from the shared `walkthrough.*` family). Matching the whole string
    // would silently drop such a row out of this walk — which is the exact
    // dead-row blindness the walk exists to prevent.
  const declared = KBC_ACTIVE_COMMANDS.filter(
    (c) => c.scope === "board" && (c.when ?? "").startsWith("board == browser"),
  );

  it("has at least one row (a scope with no rows is a dead scope)", () => {
    expect(declared.length).toBeGreaterThan(0);
  });

  /// The v7.0 defect, forwards: a row shipped in the registry that nothing
  /// on this page runs.
  it("every board==browser registry row is owned by the page", () => {
    for (const c of declared) {
      expect(
        (BROWSER_COMMAND_IDS as readonly string[]).includes(c.id),
        `${c.id} is in registry.json but nothing on ~browser handles it`,
      ).toBe(true);
    }
  });

  /// And backwards: a handler for a command that no longer exists, whose
  /// key nobody can press.
  it("every id the page claims exists in the registry", () => {
    const ids = new Set(declared.map((c) => c.id));
    for (const id of BROWSER_COMMAND_IDS) {
      expect(ids.has(id), `${id} has a handler but no registry row`).toBe(true);
    }
  });

  it("every row is surface-dispatched, shipped, owned by browser, and non-mutating", () => {
    for (const c of declared) {
      expect(c.dispatch, `${c.id} must be surface-dispatched`).toBe("surface");
      expect(c.lifecycle, `${c.id} must be shipped or retired`).toBe("shipped");
      expect(c.mutation, `${c.id} must not mutate`).toBe("none");
      expect(c.owner, c.id).toBe("browser");
    }
  });
});
