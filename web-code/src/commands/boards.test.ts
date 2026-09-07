// V74-L2 — the kbc-canvas/1 board rows' own guard.
//
// `deadRows.test.ts` closes the "shipped CENTRAL row with no executor" hole.
// Nineteen of this unit's twenty-one rows are `dispatch: "surface"` (the board
// route renders the thing, so the route owns execution — the two-layer rule),
// so that suite does not cover them. This one asks the same mechanical
// question for exactly this family, plus the three things `web-code/CLAUDE.md`'s
// keyboard section says to check before trusting a new bare key: does it
// RESOLVE where it should, does it stay out of the CM6 buffer's way, and is it
// provably disjoint from every other row on the same key.
//
// The `board` SCOPE is shared with three older sub-surfaces (`browser`,
// `canvas`, `player`, `lens`), which is why every row here is gated on
// `board == boards`: same scope, provably disjoint `when`, no conflict — the
// mechanism `board.row-next` (`board == browser`) and `lens.row-next`
// (`board == lens`) have used since v7.0.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { commandsForScope, resolve, whenDisjoint } from "./dispatch";
import { shouldWithholdFromBuffer } from "./CommandRoot";
import { KBC_COMMANDS } from "./registry.gen";

/// Every row this unit adds, with the key it is authored on and the context it
/// needs. Written out rather than derived from the registry: this table is the
/// CLAIM, and the registry is what it is checked against.
const READING = { board: "boards" } as const;
const WALKING = { board: "boards", walkthrough: true } as const;

const BOARD_ROWS: ReadonlyArray<[string, string, Record<string, string | boolean>]> = [
  ["boards.card-next", "j", READING],
  ["boards.card-prev", "k", READING],
  ["boards.card-open", "Enter", READING],
  ["boards.fold", "z c", READING],
  ["boards.unfold", "z o", READING],
  ["boards.fold-toggle", "z a", READING],
  ["boards.fold-all", "z M", READING],
  ["boards.unfold-all", "z R", READING],
  ["boards.context-more", "+", READING],
  ["boards.context-less", "-", READING],
  ["boards.thread", "Space b t", READING],
  ["boards.pin", "Space b p", READING],
  ["boards.unpin", "Space b u", READING],
  ["boards.accept", "Space b A", READING],
  ["boards.sweep", "Space b s", READING],
  ["boards.walkthrough", "p", READING],
  ["walkthrough.next", "n", WALKING],
  ["walkthrough.prev", "k", WALKING],
  ["walkthrough.play", "p", WALKING],
];

/// The two `scope: "global"` rows, which `deadRows.test.ts` DOES cover for
/// registration; listed here for the resolve + CLI checks.
const GLOBAL_ROWS: ReadonlyArray<[string, string]> = [
  ["nav.boards", "Space g w"],
  ["boards.add", "Space b a"],
];

const BOARD_DETAIL_SRC = readFileSync(
  fileURLToPath(new URL("../routes/BoardDetail.tsx", import.meta.url)),
  "utf-8",
);
const READER_SRC = readFileSync(
  fileURLToPath(new URL("../routes/Reader.tsx", import.meta.url)),
  "utf-8",
);
const APP_SRC = readFileSync(fileURLToPath(new URL("../app.tsx", import.meta.url)), "utf-8");

describe("the board rows", () => {
  it("every row is shipped, `board`-scoped, surface-dispatched and `boards`-gated", () => {
    for (const [id] of BOARD_ROWS) {
      const row = KBC_COMMANDS.find((c) => c.id === id);
      expect(row, `${id} is missing from the registry`).toBeDefined();
      expect(row?.lifecycle).toBe("shipped");
      expect(row?.scope).toBe("board");
      expect(row?.dispatch).toBe("surface");
      // A board-scope row must never claim a `vim_kind` — `commands doctor`'s
      // check 8 refuses it (only `reader`/`global` may), and the CM6 buffer
      // would then be a second executor for a key this page owns.
      expect(row?.vimKind).toBeUndefined();
      expect(row?.when, `${id} must gate on the boards surface`).toContain("board == boards");
    }
  });

  it("every row resolves to itself under its own context", () => {
    for (const [id, key, ctx] of BOARD_ROWS) {
      expect(resolve(key, "board", ctx, "vim")?.id, `${key} with ${JSON.stringify(ctx)}`).toBe(id);
    }
  });

  it("NOT ONE of them resolves on a board surface that is not this one", () => {
    // `~canvas`, `~browser`, `~lens` and the tour player share the `board`
    // scope; a row leaking across would give one key two meanings on two
    // surfaces with no mode indicator — the recon's own worst case.
    for (const surface of ["browser", "canvas", "player", "lens"]) {
      for (const [id, key] of BOARD_ROWS) {
        const hit = resolve(key, "board", { board: surface }, "vim");
        expect(hit?.id === id, `${key} leaked onto ~${surface}`).toBe(false);
      }
    }
  });

  it("the reading half and the walking half are PROVABLY disjoint", () => {
    // `k` means "previous card" while reading and "previous step" while
    // walking; `p` means "enter the walkthrough" and "play/pause". Neither
    // pair is ratified, and neither needs to be, because `when_disjoint` can
    // prove `!walkthrough` and `walkthrough` cannot both hold.
    const byId = (id: string) => KBC_COMMANDS.find((c) => c.id === id);
    for (const [a, b] of [
      ["boards.card-prev", "walkthrough.prev"],
      ["boards.walkthrough", "walkthrough.play"],
    ]) {
      expect(whenDisjoint(byId(a)?.when, byId(b)?.when), `${a} vs ${b}`).toBe(true);
    }
    // …and the reading half really is inert once the walkthrough starts.
    expect(resolve("k", "board", WALKING, "vim")?.id).toBe("walkthrough.prev");
    expect(resolve("k", "board", READING, "vim")?.id).toBe("boards.card-prev");
  });

  it("NONE of them resolves in `reader` scope — the buffer never owns them", () => {
    // The reader keeps its OWN meaning for these keys (`j` is `move.down`,
    // `p` is `pane.pin`, `+` is `peek.context-more`); what must never happen is
    // a board row answering there, which is what a missing `scope` or a
    // widened `when` would produce.
    for (const [id, key] of BOARD_ROWS) {
      const hit = resolve(key, "reader", { board: "boards", walkthrough: true }, "vim");
      expect(hit?.id, `${key} leaked into reader scope`).not.toBe(id);
    }
  });

  it("`z` stays a PURE-vim prefix inside the CM6 buffer", () => {
    // `z c`/`z o`/`z a`/`z M`/`z R` are BOARD scope, which
    // `shouldWithholdFromBuffer` (which resolves in `"reader"`) never sees —
    // so `z` is still withheld outright, exactly as diff v2's own `z` folds
    // left it.
    const target = { closest: (sel: string) => (sel === ".kbc-codeview" ? {} : null) };
    expect(
      shouldWithholdFromBuffer(target as unknown as EventTarget, "z", 0, { board: "boards" }),
    ).toBe(true);
  });

  it("`p` and `n` are not withheld from the buffer BY THIS UNIT", () => {
    // Both are vim keys in reader scope (`pane.pin`, `find.next`), so guard 2
    // withholds them there — and must keep doing so. The assertion is that
    // adding a board-scope row on the same letter changed nothing about the
    // buffer's answer.
    const target = { closest: (sel: string) => (sel === ".kbc-codeview" ? {} : null) };
    for (const key of ["p", "n"]) {
      expect(
        shouldWithholdFromBuffer(target as unknown as EventTarget, key, 0, {
          board: "boards",
          "pane.provisional": true,
        }),
        key,
      ).toBe(true);
    }
  });

  it("the board surface offers NOTHING until it publishes its context", () => {
    // `commandsForScope("board")` with an empty bag is what an unmounted
    // surface sees; every row here must be absent from it, or `~canvas`'s own
    // keys would change meaning the moment this unit shipped.
    const ids = new Set(commandsForScope("board", {}).map((c) => c.id));
    for (const [id] of BOARD_ROWS) expect(ids.has(id), id).toBe(false);
  });

  it("each row has an executor registered in BoardDetail.tsx", () => {
    // The `deadRows.test.ts` question, asked for the surface family it cannot
    // reach — narrowed to the ONE route that renders this surface.
    expect(BOARD_DETAIL_SRC).toContain("useCommandHandlers");
    expect(BOARD_DETAIL_SRC).toContain('useCommandScope("board", {');
    expect(BOARD_DETAIL_SRC).toContain('board: "boards",');
    for (const [id] of BOARD_ROWS) {
      expect(BOARD_DETAIL_SRC.includes(`"${id}":`), `${id} has no handler`).toBe(true);
    }
  });

  it("Escape leaves the walkthrough through the EXISTING dismiss stack", () => {
    // `dismiss.mode` (order 7, `when: mode.active`) is already the home for "a
    // tour / resize submode / canvas selection is active". A second Escape row
    // would be a second home for one keystroke, and `commands doctor`'s check 7
    // would refuse the duplicate `dismiss_order` anyway.
    expect(BOARD_DETAIL_SRC).toContain('"dismiss.mode":');
    // …and the rung's own context key is PUBLISHED, or the row would resolve
    // nowhere and Escape would silently do nothing (the dead-row failure, one
    // layer down from the registry).
    expect(BOARD_DETAIL_SRC).toContain('"mode.active": walkthrough,');
    expect(KBC_COMMANDS.filter((c) => c.id.startsWith("boards.") && c.keys.vim.includes("Escape")))
      .toHaveLength(0);
  });

  it("the two global rows are registered where their surface lives", () => {
    expect(APP_SRC).toContain('"nav.boards":');
    // `boards.add` needs a caret, so the reader is its home; on every other
    // route CommandRoot's "nothing owns it here" branch leaves the key alone.
    expect(READER_SRC).toContain('"boards.add":');
    for (const [id, key] of GLOBAL_ROWS) {
      const row = KBC_COMMANDS.find((c) => c.id === id);
      expect(row?.scope).toBe("global");
      expect(row?.dispatch).toBe("central");
      expect(resolve(key, "global", {}, "vim")?.id).toBe(id);
    }
  });

  it("every row names a real CLI twin or an honest `none:<reason>`", () => {
    // The SPA half of `commands doctor`'s check 4 — the doctor itself runs in
    // `kb-code-cli`, which this suite cannot invoke.
    for (const [id] of [...BOARD_ROWS, ...GLOBAL_ROWS.map((r) => [r[0]] as [string])]) {
      const row = KBC_COMMANDS.find((c) => c.id === id);
      const cli = row?.cli ?? "";
      if (cli.startsWith("none:")) {
        expect(cli.slice(5).trim().length, `${id}'s none: reason`).toBeGreaterThanOrEqual(4);
      } else {
        expect(cli.startsWith("kb-code "), `${id} names ${cli}`).toBe(true);
      }
    }
  });
});
