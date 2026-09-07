import { describe, expect, it } from "vitest";
import { KBC_COMMANDS } from "../commands/registry.gen";
import { RECIPE_COMMAND_IDS } from "./recipeCommands";

describe("recipe home — declaration ↔ handler walk", () => {
  const declared = KBC_COMMANDS.filter((c) => c.scope === "recipe");

  it("has at least one row (a scope with no rows is a dead scope)", () => {
    expect(declared.length).toBeGreaterThan(0);
  });

  /// The v7.0 defect, forwards: a row shipped in the registry that nothing
  /// on this page runs.
  it("every scope:recipe registry row is owned by the page", () => {
    for (const c of declared) {
      expect(
        (RECIPE_COMMAND_IDS as readonly string[]).includes(c.id),
        `${c.id} is in registry.json but nothing on the recipe home handles it`,
      ).toBe(true);
    }
  });

  /// And backwards: a handler for a command that no longer exists.
  it("every id the page claims exists in the registry", () => {
    const ids = new Set(declared.map((c) => c.id));
    for (const id of RECIPE_COMMAND_IDS) {
      expect(ids.has(id), `${id} has a handler but no registry row`).toBe(true);
    }
  });

  /// These rows are owned by the page's own input, not by `CommandRoot`'s
  /// window listener — the auto-form's text/number/enum inputs would
  /// otherwise swallow a central dispatch attempt the moment a field has
  /// focus (guard 1, `isTypingTarget`).
  it("every row is surface-dispatched, shipped, and non-mutating", () => {
    for (const c of declared) {
      expect(c.dispatch, `${c.id} must be surface-dispatched`).toBe("surface");
      expect(c.lifecycle, `${c.id} must be shipped or removed`).toBe("shipped");
      expect(c.mutation, `${c.id} must not mutate`).toBe("none");
    }
  });

  /// Every row must be reachable while an auto-form field has focus — a
  /// bare printable key would be typed into a string/int/enum param input
  /// instead of firing (the same "structurally unreachable" shape V70-K1
  /// fixed for the CM6 buffer, and `searchCommands.test.ts`'s own guard for
  /// the query box).
  it("no row binds a key that would be swallowed by a form input", () => {
    const typeable = /^[!-~]$/;
    for (const c of declared) {
      for (const preset of ["vim", "plain", "helix"] as const) {
        for (const k of c.keys[preset] ?? []) {
          expect(
            typeable.test(k),
            `${c.id} binds bare ${k} — typing it would go into a form field`,
          ).toBe(false);
        }
      }
    }
  });
});
