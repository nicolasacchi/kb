// V73-K2a — the diff-v2 rows' own guard.
//
// `deadRows.test.ts` closes the "shipped CENTRAL row with no executor"
// hole. Diff v2's twelve rows are `dispatch: "surface"` (the review diff
// renders the thing, so the route owns execution — the two-layer rule), so
// that suite does not cover them. This one asks the same mechanical
// question for exactly this family, plus the two things
// `web-code/CLAUDE.md`'s keyboard section says to check before trusting a
// new bare key: does it RESOLVE where it should, and does it stay out of
// the CM6 buffer's way.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { continuations, keysFor, resolve } from "./dispatch";
import { shouldWithholdFromBuffer } from "./CommandRoot";
import { KBC_COMMANDS } from "./registry.gen";

/// Diff-v2 surface rows, with the key each is authored on. Written out
/// rather than derived from the registry: this table is the CLAIM, and the
/// registry is what it is checked against. V76-R2c added the last three.
const DIFF_V2_ROWS: ReadonlyArray<[string, string]> = [
  ["diff.hunk-viewed", "Space h"],
  ["diff.fold", "z c"],
  ["diff.unfold", "z o"],
  ["diff.fold-toggle", "z a"],
  ["diff.context-cycle", "Space c"],
  ["diff.noise-toggle", "Space n"],
  ["diff.map-toggle", "Space m"],
  ["diff.ps-next", "] p"],
  ["diff.ps-prev", "[ p"],
  ["diff.drafts", "Space w"],
  ["diff.publish", "Space W"],
  ["diff.drafts-discard", "Space X"],
  ["diff.section-toggle", "z v"],
  ["diff.collapse-viewed", "z V"],
  ["diff.expand-all", "z O"],
];

const REVIEW_DIFF_SRC = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDiff.tsx", import.meta.url)),
  "utf-8",
);

describe("diff v2's registry rows", () => {
  it("every row is shipped, `diff`-scoped and surface-dispatched", () => {
    for (const [id] of DIFF_V2_ROWS) {
      const row = KBC_COMMANDS.find((c) => c.id === id);
      expect(row, `${id} is missing from the registry`).toBeDefined();
      expect(row?.lifecycle).toBe("shipped");
      expect(row?.scope).toBe("diff");
      expect(row?.dispatch).toBe("surface");
      // A diff-scope row must never claim a vim_kind — `commands doctor`'s
      // check 8 refuses it, and the CM6 buffer would then be a second
      // executor for a key this page owns.
      expect(row?.vimKind).toBeUndefined();
    }
  });

  it("every row resolves to itself in `diff` scope on the authored key", () => {
    for (const [id, key] of DIFF_V2_ROWS) {
      expect(resolve(key, "diff", {}, "vim")?.id, `${key} in diff scope`).toBe(id);
    }
  });

  it("NONE of them resolves in `reader` scope — the buffer never owns them", () => {
    for (const [, key] of DIFF_V2_ROWS) {
      const hit = resolve(key, "reader", {}, "vim");
      expect(hit === null || hit.scope === "global", `${key} leaked into reader scope`).toBe(true);
    }
  });

  it("`z` stays a PURE-vim prefix inside the CM6 buffer", () => {
    // The rule from `web-code/CLAUDE.md`: guard 2 withholds a prefix only
    // when EVERY reachable continuation in READER scope carries a
    // `vim_kind`. Diff v2 adds `z c`/`z o`/`z a` in DIFF scope, which
    // `resolve`/`continuations` never see from the reader — so `z` must
    // still be withheld outright, exactly as before this unit.
    const target = { closest: (sel: string) => (sel === ".kbc-codeview" ? {} : null) };
    expect(shouldWithholdFromBuffer(target as unknown as EventTarget, "z", 0, {})).toBe(true);
    const cont = continuations(["z"], "reader", {}, "vim");
    expect(cont.length).toBeGreaterThan(0);
    expect(cont.every((c) => c.command.vimKind !== undefined)).toBe(true);
  });

  it("`[`/`]` stay MIXED prefixes, so `] p`/`[ p` need no vim arm", () => {
    // `] u`/`] d`/`] s`'s own precedent — stated in CLAUDE.md and re-checked
    // here because adding a vim arm would double-fire the step.
    const target = { closest: (sel: string) => (sel === ".kbc-codeview" ? {} : null) };
    for (const prefix of ["[", "]"]) {
      expect(shouldWithholdFromBuffer(target as unknown as EventTarget, prefix, 0, {})).toBe(false);
    }
  });

  it("each row has an executor registered in ReviewDiff.tsx", () => {
    // The `deadRows.test.ts` question, asked for the surface family it
    // cannot reach. Narrowed to the ONE route that renders this surface,
    // which is the stronger form that suite says it lacks.
    expect(REVIEW_DIFF_SRC).toContain("useCommandHandlers");
    for (const [id] of DIFF_V2_ROWS) {
      expect(REVIEW_DIFF_SRC.includes(`"${id}":`), `${id} has no handler`).toBe(true);
    }
  });

  it("V76-R2c keys collide with nothing and are not a leader PREFIX either way", () => {
    const ids = ["diff.section-toggle", "diff.collapse-viewed", "diff.expand-all"] as const;
    for (const preset of ["vim", "plain", "helix"] as const) {
      const byKey = new Map<string, string[]>();
      for (const c of KBC_COMMANDS) {
        for (const k of keysFor(c, preset)) {
          byKey.set(k, [...(byKey.get(k) ?? []), c.id]);
        }
      }
      for (const id of ids) {
        const row = KBC_COMMANDS.find((c) => c.id === id);
        expect(row, id).toBeDefined();
        for (const k of keysFor(row!, preset)) {
          expect(byKey.get(k), `${preset} ${k}`).toEqual([id]);
          const toks = k.split(" ");
          for (let i = 1; i < toks.length; i += 1) {
            const prefix = toks.slice(0, i).join(" ");
            expect(byKey.has(prefix), `${preset}: ${k} is shadowed by ${prefix}`).toBe(false);
          }
          for (const other of byKey.keys()) {
            if (other !== k) {
              expect(other.startsWith(`${k} `), `${preset}: ${k} would shadow ${other}`).toBe(false);
            }
          }
        }
      }
    }
  });

  it("every row names a real CLI twin or an honest `none:<reason>`", () => {
    // The SPA half of `commands doctor`'s check 4 — the doctor itself runs
    // in `kb-code-cli`, which this suite cannot invoke.
    for (const [id] of DIFF_V2_ROWS) {
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
