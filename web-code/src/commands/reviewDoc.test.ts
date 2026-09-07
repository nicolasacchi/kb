// The Review Room's registry rows (V73-K2b) — the narrowed, per-surface form
// of `deadRows.test.ts`, in the shape `diffV2.test.ts` established.
//
// `deadRows.test.ts` structurally cannot reach these: it gates `central` rows
// only, and every cockpit row is `dispatch: "surface"` (the route that
// renders the tab owns execution). So this file is what stops a row shipping
// with no handler — the "silently dead row" failure web-code/CLAUDE.md's
// keyboard section is written to prevent.
//
// It also pins the reason `6` is safe as a bare digit and the reason `] r`/
// `[ r` carry no `vim_kind`.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { KBC_COMMANDS } from "./registry.gen";
import { continuations, resolve } from "./dispatch";

const REVIEW_DETAIL = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDetail.tsx", import.meta.url)),
  "utf8",
);

/** Every row this unit ships or wires, with the key it binds in `vim`. */
const COCKPIT_ROWS: ReadonlyArray<[string, string]> = [
  ["review.tab.report", "1"],
  ["review.tab.files", "2"],
  ["review.tab.map", "3"],
  ["review.tab.order", "4"],
  ["review.tab.timeline", "5"],
  ["review.tab.doc", "6"],
  ["doc.cards-fold", "Space r"],
  ["doc.card-next", "] r"],
  ["doc.card-prev", "[ r"],
  ["doc.card-open", "Space o"],
  ["doc.compose-copy", "Space y"],
];

function row(id: string) {
  return KBC_COMMANDS.find((c) => c.id === id);
}

describe("the Review Room's registry rows", () => {
  it("ships every row in review scope, on the surface, with no vim_kind", () => {
    for (const [id] of COCKPIT_ROWS) {
      const c = row(id);
      expect(c, `${id} is not in the generated registry`).toBeTruthy();
      expect(c?.lifecycle, `${id} lifecycle`).toBe("shipped");
      expect(c?.scope, `${id} scope`).toBe("review");
      expect(c?.dispatch, `${id} dispatch`).toBe("surface");
      // The cockpit mounts no CodeView, so no row here may claim a vim arm —
      // `shouldWithholdFromBuffer` resolves in `"reader"` and would never see
      // one anyway, and an arm would fire the handler twice.
      expect(c?.vimKind, `${id} must carry no vim_kind`).toBeUndefined();
    }
  });

  it("resolves every key in review scope", () => {
    for (const [id, key] of COCKPIT_ROWS) {
      const hit = resolve(key, "review", {}, "vim");
      expect(hit?.id, `${key} should resolve to ${id}`).toBe(id);
    }
  });

  it("registers a handler for every row it ships", () => {
    // The same substring test `deadRows.test.ts` uses, narrowed to the ONE
    // route that owns these ids.
    expect(REVIEW_DETAIL).toContain("useCommandHandlers");
    for (const [id] of COCKPIT_ROWS) {
      expect(REVIEW_DETAIL, `${id} has no handler in ReviewDetail.tsx`).toContain(`"${id}":`);
    }
  });

  it("publishes the review scope, which nothing did before this unit", () => {
    expect(REVIEW_DETAIL).toContain('useCommandScope("review"');
  });

  it("names a real CLI twin or an honest none:<reason>", () => {
    for (const [id] of COCKPIT_ROWS) {
      const cli = row(id)?.cli ?? "";
      if (cli.startsWith("none:")) {
        expect(cli.slice(5).trim().length, `${id}: none: with no reason`).toBeGreaterThanOrEqual(4);
      } else {
        expect(cli.startsWith("kb-code "), `${id}: ${cli} is not a kb-code verb`).toBe(true);
      }
    }
  });

  it("keeps `[`/`]` a MIXED prefix — which is why ] r / [ r carry no vim arm", () => {
    // `[c`/`[f` are vim's, `[d`/`]d`/`]p`/`]u`/`]s` are not. A vim arm on a
    // mixed prefix's continuation fires the step TWICE (V71-K4).
    for (const prefix of ["[", "]"]) {
      const conts = continuations([prefix], "reader", {}, "vim");
      expect(conts.length, `${prefix} should have continuations`).toBeGreaterThan(0);
      expect(
        conts.some((c) => c.command.vimKind !== undefined),
        `${prefix} should still have at least one vim continuation`,
      ).toBe(true);
      expect(
        conts.some((c) => c.command.vimKind === undefined),
        `${prefix} should still have at least one non-vim continuation`,
      ).toBe(true);
    }
  });

  it("does not leak a review-scope key into the reader", () => {
    // `review` and `reader` are coactive at depth 20; a row that ALSO
    // resolved in reader scope would be a conflict the doctor gates.
    for (const [, key] of COCKPIT_ROWS) {
      const hit = resolve(key, "reader", {}, "vim");
      if (hit) expect(hit.scope, `${key} resolves in reader as ${hit.id}`).toBe("global");
    }
  });
});
