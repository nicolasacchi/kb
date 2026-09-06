// V71-K2/K3 — the recurrence guard for kb-code's worst defect class: a shipped
// registry row that NOTHING executes.
//
// `web-code/CLAUDE.md`'s keyboard section states the rule and names the gap
// this file closes: "A registry row with no registered handler is silently
// dead. … Whenever you add a row whose `dispatch` is `central`, grep for a
// `useCommandHandlers({ "<id>": … })` call that actually registers it before
// you trust the key does anything — there is no test today that a shipped
// central row has a registered handler in every reachable route (a real gap;
// a future unit's job, not this one's)." This is that test, minus the "in
// every reachable route" half, which needs a route-graph this suite does not
// have: it asks the weaker, mechanical question — is the id registered
// ANYWHERE at all? — because the answer was NO for eighteen shipped rows,
// including the whole `Space`-leader desk/drawer/rail family, and nothing
// said so. `CommandRoot.onKey`'s own "matched" branch is why it is silent:
// `if (!handler) return; // nothing owns it here — leave the key alone`.
//
// The failure mode is not hypothetical. `V70-A5` shipped five `pane.*` rows
// with no handler (`V70-H1` found them); `V70-A4` shipped `desk.toggle.drawer`
// with none, and `V70-K1`'s own browser spec for the Space leader
// (`day-one.spec.ts`) was written against it and shipped RED — the guard it
// fixed was correct, the key it pressed had no owner.
//
// HOW TO USE THE ALLOW-LIST: it is a debt ledger, not a config. Shrinking it
// (by registering the handler) is the fix; a row may only be ADDED to it with
// a reason, and adding one is a deliberate act a reviewer can see. It is
// pinned exactly — a row that gets wired but stays listed fails too, so the
// ledger cannot rot in the other direction either.
//
// V71-K3 emptied the ledger: the seventeen rows V71-K2 found and left
// (`desk.preset.*`, the whole `drawer.*` family, `rail.pin` and
// `rail.tab.*`) each now dispatch exactly the call its own mouse affordance
// already made — see `routes/Reader.tsx`'s `useCommandHandlers` block (the
// V71-K3-tagged entries right after `desk.toggle.drawer`) for each one's own
// doc. `KNOWN_UNREGISTERED` stays in the file, empty, rather than being
// deleted: the shape of the guard — "the live set must equal the pinned
// ledger, by name" — is what makes a FUTURE dead row fail loudly and
// nameably (`unregisteredCentralRows()`'s diff against `[]` lists exactly
// the new id), not a comment promising a human will notice.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { KBC_COMMANDS } from "./registry.gen";

const SRC = fileURLToPath(new URL("..", import.meta.url));

/// Shipped `dispatch: "central"` rows with no `useCommandHandlers`
/// registration anywhere in the SPA. Empty as of V71-K3 — see this file's
/// header. A row may only be ADDED back with a reason, and doing so is a
/// deliberate act a reviewer can see; the ledger is pinned EXACTLY (below),
/// so a row that gets wired but stays listed fails too.
const KNOWN_UNREGISTERED: readonly string[] = [];

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (/\.tsx?$/.test(name) && !/\.test\.tsx?$/.test(name)) out.push(p);
  }
  return out;
}

/// Every source file that registers handlers at all, concatenated. Narrowing
/// to `useCommandHandlers` callers first is what keeps `"<id>":` from
/// matching an unrelated object literal elsewhere in the app.
function registrationText(): string {
  return walk(SRC)
    .map((p) => readFileSync(p, "utf-8"))
    .filter((body) => body.includes("useCommandHandlers"))
    .join("\n");
}

function unregisteredCentralRows(): string[] {
  const text = registrationText();
  return KBC_COMMANDS.filter(
    (c) => c.dispatch === "central" && c.lifecycle === "shipped" && !text.includes(`"${c.id}":`),
  )
    .map((c) => c.id)
    .sort();
}

describe("shipped central rows have an executor", () => {
  it("no shipped `dispatch: central` row is dead — the ledger is EMPTY (V71-K3)", () => {
    // The ledger closed to zero this unit; a future row that ships with no
    // registered handler fails HERE, and vitest's own array diff names it —
    // no separate "which row" step required.
    expect(unregisteredCentralRows()).toEqual([...KNOWN_UNREGISTERED].sort());
  });

  it("`desk.toggle.drawer` (`Space d`) is registered — the V70-K1 spec's own key", () => {
    // Named on its own so the regression reads as itself rather than as a
    // diff in an array: `day-one.spec.ts:196` presses `Space d` from
    // inside a focused CM6 buffer, and the guard V70-K1 fixed only gets the
    // key as far as a handler that has to exist.
    expect(unregisteredCentralRows()).not.toContain("desk.toggle.drawer");
  });

  it("none of V71-K3's seventeen rows regress back to dead", () => {
    // Named individually (rather than trusting the empty-ledger check
    // alone) so a regression on any ONE of these reads as itself in a
    // failing test name, not as a line in a diff.
    const dead = new Set(unregisteredCentralRows());
    const K3_ROWS = [
      "desk.preset.explore",
      "desk.preset.present",
      "desk.preset.read",
      "desk.preset.review",
      "drawer.close",
      "drawer.keep",
      "drawer.pin",
      "drawer.reopen",
      "drawer.tab",
      "drawer.tab-next",
      "drawer.tab-prev",
      "rail.pin",
      "rail.tab.all",
      "rail.tab.history",
      "rail.tab.notes",
      "rail.tab.review",
      "rail.tab.understand",
    ];
    for (const id of K3_ROWS) expect(dead.has(id)).toBe(false);
  });

  it("the ledger names only rows that really are in the registry", () => {
    // A stale entry (a row renamed or retired) would silently weaken the
    // check above, so the ledger is validated against the registry too.
    const ids = new Set(KBC_COMMANDS.map((c) => c.id));
    expect(KNOWN_UNREGISTERED.filter((id) => !ids.has(id))).toEqual([]);
  });
});
