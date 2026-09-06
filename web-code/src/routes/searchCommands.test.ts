import { describe, expect, it } from "vitest";
import { KBC_COMMANDS } from "../commands/registry.gen";
import { SEARCH_COMMAND_IDS } from "./searchCommands";

describe("search results page — declaration ↔ handler walk", () => {
  const declared = KBC_COMMANDS.filter((c) => c.scope === "search");

  it("has at least one row (a scope with no rows is a dead scope)", () => {
    expect(declared.length).toBeGreaterThan(0);
  });

  /// The v7.0 defect, forwards: a row shipped in the registry that nothing
  /// on this page runs.
  it("every scope:search registry row is owned by the page", () => {
    for (const c of declared) {
      expect(
        (SEARCH_COMMAND_IDS as readonly string[]).includes(c.id),
        `${c.id} is in registry.json but nothing on the results page handles it`,
      ).toBe(true);
    }
  });

  /// And backwards: a handler for a command that no longer exists, whose
  /// key nobody can press.
  it("every id the page claims exists in the registry", () => {
    const ids = new Set(declared.map((c) => c.id));
    for (const id of SEARCH_COMMAND_IDS) {
      expect(ids.has(id), `${id} has a handler but no registry row`).toBe(true);
    }
  });

  /// These rows are owned by the page's own input, not by `CommandRoot`'s
  /// window listener — the palette pattern. A `central` row here would be
  /// silently dead the moment the query box has focus, because
  /// `isTypingTarget` (guard 1) stops the window host before it dispatches.
  it("every row is surface-dispatched and shipped", () => {
    for (const c of declared) {
      expect(c.dispatch, `${c.id} must be surface-dispatched`).toBe("surface");
      expect(c.lifecycle, `${c.id} must be shipped or removed`).toBe("shipped");
      expect(c.mutation, `${c.id} must not mutate`).toBe("none");
    }
  });

  /// Every row must be REACHABLE while the query box has focus, i.e. its
  /// key must survive a text input. A bare printable key would be typed
  /// into the query instead of firing — the same "structurally
  /// unreachable" shape V70-K1 fixed for the buffer.
  it("no row binds a key that would be swallowed by the query input", () => {
    const typeable = /^[!-~]$/;
    for (const c of declared) {
      for (const preset of ["vim", "plain", "helix"] as const) {
        for (const k of c.keys[preset] ?? []) {
          expect(
            typeable.test(k),
            `${c.id} binds bare ${k} — typing it would go into the query box`,
          ).toBe(false);
        }
      }
    }
  });
});
