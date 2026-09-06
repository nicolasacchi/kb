// V70-A5 — the reader buffer's vocabulary and the registry are ONE list.
//
// `editor/vimKeys.ts` is the pure reducer that owns the CM6 buffer, and its
// `VIM_COMMAND_KINDS` array is the closest thing kb-code had to an action-id
// vocabulary before this unit. It stays the executor for reader-scope
// commands (the two-layer rule: a chord machine scoped to one EditorView is
// the right home for buffer motions, and a window listener is not) — but it
// is no longer a SECOND vocabulary. Every kind is now named by at least one
// registry row, and every row that claims a kind names a real one.
//
// Both directions matter. Forward catches a typo'd `vim_kind` in the
// registry. Backward catches the real regression: a new `VimCommand` variant
// shipped with a key nobody wrote down, which is exactly how a cheat sheet
// starts lying.
import { describe, expect, it } from "vitest";
import { VIM_COMMAND_KINDS } from "../editor/vimKeys";
import { KBC_COMMANDS } from "./registry.gen";

const kinds = new Set<string>(VIM_COMMAND_KINDS);
const claimed = KBC_COMMANDS.flatMap((c) => (c.vimKind ? [c.vimKind] : []));

describe("vimKeys ↔ kbc-cmd/1 parity", () => {
  it("every registry vim_kind is a real VimCommand kind", () => {
    for (const c of KBC_COMMANDS) {
      if (!c.vimKind) continue;
      expect(kinds, `${c.id} claims vim_kind ${c.vimKind}`).toContain(c.vimKind);
    }
  });

  it("every VimCommand kind is named by at least one registry row", () => {
    const missing = [...kinds].filter((k) => !claimed.includes(k));
    expect(
      missing,
      "these buffer commands have no registry row — add one (id, title, keys, group) " +
        "before shipping the reducer variant, or the `?` sheet and the palette " +
        "cannot see them",
    ).toEqual([]);
  });

  it("keeps every vim_kind row inside a scope the buffer can actually be in", () => {
    // A buffer command declared on, say, `branches` would never execute: the
    // CM6 layer only ever reports the reader scope (and `help.keys` is the
    // one deliberately-global row the buffer also fires, via `cb-show-help`).
    for (const c of KBC_COMMANDS) {
      if (!c.vimKind) continue;
      expect(["reader", "global"], c.id).toContain(c.scope);
    }
  });

  it("routes the reader's motions through one shared `move` kind", () => {
    const moves = KBC_COMMANDS.filter((c) => c.vimKind === "move");
    expect(moves.length).toBeGreaterThan(15);
    // Motions are the one many-to-one mapping: 20+ keys, one reducer kind.
    // Everything else is 1:1 or 1:2 (a next/prev pair sharing a step kind).
    for (const c of moves) expect(c.group === "Move" || c.group === "Marks + jumps").toBe(true);
  });
});
