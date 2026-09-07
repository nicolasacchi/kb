// V73-K2c's own registry rows — the narrowed, per-surface form of
// `deadRows.test.ts`, in the shape `diffV2.test.ts`/`reviewDoc.test.ts`
// established.
//
// `deadRows.test.ts` structurally cannot reach these: every row here is
// `dispatch: "surface"` (the route that renders the thing owns execution),
// so this file is what stops a row shipping with no handler — the
// "silently dead row" failure web-code/CLAUDE.md's keyboard section is
// written to prevent.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { continuations, resolve } from "./dispatch";
import { KBC_COMMANDS } from "./registry.gen";

const REVIEW_DETAIL_ROWS: ReadonlyArray<[string, string]> = [
  // `Space z`/`Space l` were the original picks — both already claimed
  // GLOBALLY by V72-I2's `rails.schema-fold`/`rails.atom.open`, caught only
  // by a manual full-registry scan during a rebase (`commands doctor`'s own
  // conflict pass skips any pair where either scope is `global` — see
  // `web-code/CLAUDE.md`'s own note on this).
  ["review.claims-toggle", "Space q"],
  ["review.timeline.lane-cycle", "Space j"],
  ["review.timeline.github-toggle", "Space G"],
];

const REVIEW_DIFF_ROWS: ReadonlyArray<[string, string]> = [
  ["diff.hunk-turns", "Space T"],
  ["diff.pseudo.pr-body", "Space f 1"],
  ["diff.pseudo.review-md", "Space f 2"],
  ["diff.pseudo.findings", "Space f 3"],
  ["diff.pseudo.commits", "Space f 4"],
];

const ALL_ROWS: ReadonlyArray<[string, string, "review" | "diff"]> = [
  ...REVIEW_DETAIL_ROWS.map(([id, k]) => [id, k, "review"] as [string, string, "review" | "diff"]),
  ...REVIEW_DIFF_ROWS.map(([id, k]) => [id, k, "diff"] as [string, string, "review" | "diff"]),
];

const REVIEW_DETAIL_SRC = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDetail.tsx", import.meta.url)),
  "utf8",
);
const REVIEW_DIFF_SRC = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDiff.tsx", import.meta.url)),
  "utf8",
);

function row(id: string) {
  return KBC_COMMANDS.find((c) => c.id === id);
}

describe("V73-K2c's registry rows (timeline v2 / claims / hunk-turns / pseudo-files)", () => {
  it("ships every row shipped, on the surface, with no vim_kind", () => {
    for (const [id, , scope] of ALL_ROWS) {
      const c = row(id);
      expect(c, `${id} is not in the generated registry`).toBeTruthy();
      expect(c?.lifecycle, `${id} lifecycle`).toBe("shipped");
      expect(c?.scope, `${id} scope`).toBe(scope);
      expect(c?.dispatch, `${id} dispatch`).toBe("surface");
      // Neither ReviewDetail.tsx nor ReviewDiff.tsx mounts a CodeView, so
      // `shouldWithholdFromBuffer` (which resolves in "reader") never sees
      // any of these — the same reason the pre-existing diff-scope rows
      // are safe, and a vim arm would fire the handler twice for nothing.
      expect(c?.vimKind, `${id} must carry no vim_kind`).toBeUndefined();
    }
  });

  it("resolves every key in its own declared scope", () => {
    for (const [id, key, scope] of ALL_ROWS) {
      const hit = resolve(key, scope, {}, "vim");
      expect(hit?.id, `${key} in ${scope} scope should resolve to ${id}`).toBe(id);
    }
  });

  it("registers a handler for every row it ships", () => {
    expect(REVIEW_DETAIL_SRC).toContain("useCommandHandlers");
    for (const [id] of REVIEW_DETAIL_ROWS) {
      expect(REVIEW_DETAIL_SRC, `${id} has no handler in ReviewDetail.tsx`).toContain(`"${id}":`);
    }
    expect(REVIEW_DIFF_SRC).toContain("useCommandHandlers");
    for (const [id] of REVIEW_DIFF_ROWS) {
      expect(REVIEW_DIFF_SRC, `${id} has no handler in ReviewDiff.tsx`).toContain(`"${id}":`);
    }
  });

  it("names a real CLI twin or an honest none:<reason>", () => {
    for (const [id] of ALL_ROWS) {
      const cli = row(id)?.cli ?? "";
      if (cli.startsWith("none:")) {
        expect(cli.slice(5).trim().length, `${id}: none: with no reason`).toBeGreaterThanOrEqual(4);
      } else {
        expect(cli.startsWith("kb-code "), `${id}: ${cli} is not a kb-code verb`).toBe(true);
      }
    }
  });

  it("does not leak into the reader scope (review/diff are coactive with reader at depth 20)", () => {
    for (const [, key] of ALL_ROWS) {
      const hit = resolve(key, "reader", {}, "vim");
      if (hit) expect(hit.scope, `${key} resolves in reader as ${hit.id}`).toBe("global");
    }
  });

  it("the four pseudo rows share the `Space f` prefix WITHIN diff scope only — no cross-scope collision", () => {
    // `rail.pin` (global) is the bare `Space p` binding; `Space f` is a
    // genuinely unclaimed prefix anywhere else in the registry, verified
    // here by walking every continuation of it in `diff` scope and
    // confirming they are ALL our four rows.
    const conts = continuations(["Space", "f"], "diff", {}, "vim");
    const ids = new Set(conts.map((c) => c.command.id));
    expect(ids).toEqual(
      new Set([
        "diff.pseudo.pr-body",
        "diff.pseudo.review-md",
        "diff.pseudo.findings",
        "diff.pseudo.commits",
      ]),
    );
  });
});
