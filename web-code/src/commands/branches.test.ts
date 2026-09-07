import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { IDLE, keysFor, resolve, step, tokensOf } from "./dispatch";
import { KBC_COMMANDS, KBC_SCOPES, type KbcPreset } from "./registry.gen";

// V75-M3 — the same whole-registry prefix scan `tours.test.ts` established
// as the permanent form (V72-I2 and V73-K2c both shipped a collision
// `commands doctor` structurally cannot see), applied to the `~branches`
// rows.
//
// ONE deliberate split, and it is the whole reason this file is not a copy
// of `tours.test.ts`:
//
//   * The SIX rows V75-M3 adds bind keys that are unique across the ENTIRE
//     registry, in every preset. They get the full scan — exact uniqueness,
//     no complete binding is a prefix of theirs, and theirs is a prefix of
//     nobody's.
//   * The FOUR rows V75-M3 merely SHIPS (`j`/`k`/`Enter`/`r`, declared
//     `planned` since V4.L1) deliberately share their keys with the other
//     depth-20 list surfaces — that is the design (`j` is "next row"
//     everywhere), and it is why each already carries a full
//     `ratified_conflicts` list. Asserting uniqueness for them would be
//     asserting the opposite of the ratified decision, so they get the
//     check that IS meaningful: every collision is ratified from BOTH
//     sides. (`kb-code commands doctor` gates that too; this pins it at the
//     row level, where a future edit to one side is visible.)

const NEW_IDS = [
  "branches.view-next",
  "branches.view-prev",
  "branches.favourite",
  "branches.radar",
  "branches.fold-prefix",
  "branches.density",
] as const;

const SHIPPED_RATIFIED_IDS = [
  "branches.next",
  "branches.prev",
  "branches.compare",
  "branches.start-review",
] as const;

const ALL_IDS = [...NEW_IDS, ...SHIPPED_RATIFIED_IDS];
const PRESETS: KbcPreset[] = ["vim", "plain", "helix"];

function row(id: string) {
  const c = KBC_COMMANDS.find((x) => x.id === id);
  if (!c) throw new Error(`no registry row ${id}`);
  return c;
}

describe("V75-M3 registry rows", () => {
  it("every row is shipped, surface-dispatched, branch-scoped and owned", () => {
    for (const id of ALL_IDS) {
      const c = row(id);
      expect(c.lifecycle, id).toBe("shipped");
      // Surface, not central: the page owns these keys, and `CommandRoot`'s
      // window listener must not fire them from another route.
      expect(c.dispatch, id).toBe("surface");
      expect(c.scope, id).toBe("branches");
      expect(c.owner, id).toBe("branches");
      // No branch row touches the WORKING TREE. `branches.start-review` is
      // the only one that writes at all, and what it writes is a review —
      // `mutation: "metadata"` in this registry's vocabulary, which is the
      // honest label and the reason it is gated on `loopback` at the
      // affordance too.
      expect(["none", "metadata"], `${id} mutation`).toContain(c.mutation);
    }
  });

  it("carries the RATIFIED keys, identically in all three presets", () => {
    const want: Record<string, string> = {
      "branches.view-next": "L",
      "branches.view-prev": "H",
      "branches.favourite": "P",
      "branches.radar": "R",
      "branches.fold-prefix": "Z",
      "branches.density": "D",
    };
    for (const id of NEW_IDS) {
      for (const preset of PRESETS) {
        expect(keysFor(row(id), preset), `${id}/${preset}`).toEqual([want[id]]);
      }
    }
  });

  it("never binds a key the LEADER family owns — no `Space`-prefixed row here", () => {
    for (const id of ALL_IDS) {
      for (const preset of PRESETS) {
        for (const k of keysFor(row(id), preset)) {
          expect(k.startsWith("Space"), `${id}/${preset}: ${k}`).toBe(false);
        }
      }
    }
    // `Space g b` is `nav.branches` and stays there — this unit adds no
    // leader key at all, which is why the `Space g` letter space
    // (m w R T e t r … ) is untouched.
    expect(keysFor(row("nav.branches"), "vim")).toEqual(["Space g b"]);
  });

  it("the SIX new keys collide with nothing — no shared key, and no leader PREFIX either way", () => {
    for (const preset of PRESETS) {
      const byKey = new Map<string, string[]>();
      for (const c of KBC_COMMANDS) {
        for (const k of keysFor(c, preset)) {
          byKey.set(k, [...(byKey.get(k) ?? []), c.id]);
        }
      }
      for (const id of NEW_IDS) {
        for (const k of keysFor(row(id), preset)) {
          // (a) nobody else binds this exact key, in ANY scope.
          expect(byKey.get(k), `${preset} ${k}`).toEqual([id]);
          const toks = k.split(" ");
          // (b) no COMPLETE binding is a strict prefix of this chord.
          for (let i = 1; i < toks.length; i += 1) {
            const prefix = toks.slice(0, i).join(" ");
            expect(byKey.has(prefix), `${preset}: ${k} is shadowed by ${prefix}`).toBe(false);
          }
          // (c) this key is not a strict prefix of anyone ELSE's chord —
          //     the check that catches `z` (a prefix of `z z`/`z t`/…) and
          //     `g` (a prefix of `g d`/`g r`/…), which is why neither was
          //     taken for this family.
          for (const other of byKey.keys()) {
            if (other !== k) {
              expect(other.startsWith(`${k} `), `${preset}: ${k} would shadow ${other}`).toBe(false);
            }
          }
        }
      }
    }
  });

  it("the four SHIPPED-BY-THIS-UNIT rows ratify every collision from BOTH sides", () => {
    for (const preset of PRESETS) {
      const byKey = new Map<string, string[]>();
      for (const c of KBC_COMMANDS) {
        for (const k of keysFor(c, preset)) {
          byKey.set(k, [...(byKey.get(k) ?? []), c.id]);
        }
      }
      for (const id of SHIPPED_RATIFIED_IDS) {
        const mine = row(id);
        for (const k of keysFor(mine, preset)) {
          for (const otherId of byKey.get(k) ?? []) {
            if (otherId === id) continue;
            const other = row(otherId);
            // `global` vs narrower is the shadowing lint's business, not
            // the conflict gate's — the same carve-out `commands.rs`'s own
            // `conflicts()` makes.
            if (other.scope === "global" || mine.scope === "global") continue;
            // Different modal depth is a DESIGNED shadow, also not a
            // conflict. `branches` is depth 20; so are `reader`/`diff`/
            // `review`/`board`, which is exactly why these lists exist.
            // The SAME two conditions `commands.rs::conflicts()` applies —
            // read off the generated scope table rather than restated, so
            // this test cannot drift from the gate it mirrors.
            const a = KBC_SCOPES.find((s) => s.id === mine.scope);
            const b = KBC_SCOPES.find((s) => s.id === other.scope);
            if (!a || !b) continue;
            const coactive = a.coactiveWith.includes(other.scope);
            if (!coactive || a.depth !== b.depth) continue;
            const bothWays =
              (mine.ratifiedConflicts ?? []).includes(otherId) &&
              (other.ratifiedConflicts ?? []).includes(id);
            expect(bothWays, `${preset} ${k}: ${id} ↔ ${otherId} is not ratified both ways`).toBe(
              true,
            );
          }
        }
      }
    }
  });

  it("resolves to its own id in the branches scope", () => {
    for (const id of ALL_IDS) {
      const key = keysFor(row(id), "vim")[0];
      expect(resolve(key, "branches", {}, "vim")?.id, id).toBe(id);
    }
  });

  it("walks the CHORD MACHINE token by token — the last token is the match", () => {
    for (const id of NEW_IDS) {
      const key = keysFor(row(id), "vim")[0];
      const toks = tokensOf(key);
      let state = IDLE;
      let matched: string | null = null;
      for (const [i, t] of toks.entries()) {
        const out = step(state, t, "branches", {}, "vim");
        if (i < toks.length - 1) {
          expect(out.kind, `${id}: token ${t} must be pending, not ${out.kind}`).toBe("pending");
        } else {
          expect(out.kind, `${id}: ${key} must MATCH`).toBe("matched");
          if (out.kind === "matched") matched = out.command.id;
        }
        state = out.state;
      }
      expect(matched, id).toBe(id);
    }
  });
});

describe("the SPA registers a handler for each of them", () => {
  // `surfaceRows.test.ts` already asks "is it claimed anywhere"; this pins
  // the FILE, so a refactor that moves the handler map somewhere the key
  // cannot reach fails here by name rather than passing the general check.
  const OWNER = "components/branches/BranchViews.tsx";

  it("registers every branches row in the file that owns the surface", () => {
    const src = readFileSync(fileURLToPath(new URL(`../${OWNER}`, import.meta.url)), "utf8");
    for (const id of ALL_IDS) {
      expect(src.includes(`"${id}":`), `${id} in ${OWNER}`).toBe(true);
    }
  });
});
