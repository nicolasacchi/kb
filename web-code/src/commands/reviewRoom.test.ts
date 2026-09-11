// V76-R2a — the Room's seven new registry rows, in `tours.test.ts`'s shape:
// the per-surface gate `deadRows.test.ts` structurally cannot be (every row
// is `dispatch: "surface"`), plus the WHOLE-registry prefix scan that
// `commands doctor` cannot run for leader chords (its conflict pass skips
// any pair where either scope is `global`, and says nothing about a chord
// shadowed by a complete binding — see web-code/CLAUDE.md's own warning,
// shipped twice as V72-I2's `Space o` and V73-K2c's `Space z`/`Space l`).
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { KBC_COMMANDS, type KbcPreset } from "./registry.gen";
import { keysFor, resolve } from "./dispatch";

const REVIEW_DETAIL = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDetail.tsx", import.meta.url)),
  "utf8",
);

/** The seven rows this unit ships, with the key each binds in vim/helix. */
const ROOM_ROWS: ReadonlyArray<[string, string]> = [
  ["review.rail.toggle", "Space i"],
  ["review.rail.reset", "Space I"],
  ["review.density-toggle", "Space M"],
  ["review.jump.summary", "Space S"],
  ["review.jump.findings", "Space F"],
  ["review.jump.praise", "Space A"],
  ["review.jump.verdict", "Space J"],
];

const PRESETS: KbcPreset[] = ["vim", "plain", "helix"];

function row(id: string) {
  const c = KBC_COMMANDS.find((x) => x.id === id);
  if (!c) throw new Error(`no registry row ${id}`);
  return c;
}

describe("V76-R2a registry rows", () => {
  it("every row is shipped, review-scoped, surface-dispatched, vim_kind-free", () => {
    for (const [id] of ROOM_ROWS) {
      const c = row(id);
      expect(c.lifecycle, id).toBe("shipped");
      expect(c.scope, id).toBe("review");
      expect(c.dispatch, id).toBe("surface");
      // The cockpit mounts no CodeView — a vim arm would be invisible to
      // `shouldWithholdFromBuffer` (it resolves in "reader") and would fire
      // the handler twice.
      expect(c.vimKind, `${id} must carry no vim_kind`).toBeUndefined();
    }
  });

  it("carries the RATIFIED keys — and never `Space r` (that is `doc.cards-fold`)", () => {
    for (const [id, key] of ROOM_ROWS) {
      for (const preset of PRESETS) {
        expect(keysFor(row(id), preset), `${id}/${preset}`).toEqual(preset === "plain" ? [] : [key]);
      }
    }
    expect(keysFor(row("doc.cards-fold"), "vim")).toEqual(["Space r"]);
  });

  it("collides with nothing — no shared key, and no leader PREFIX either way", () => {
    // tours.test.ts's permanent whole-registry scan, run by hand because
    // the doctor cannot see a global/global or a shadowed-chord collision.
    for (const preset of PRESETS) {
      const byKey = new Map<string, string[]>();
      for (const c of KBC_COMMANDS) {
        for (const k of keysFor(c, preset)) {
          byKey.set(k, [...(byKey.get(k) ?? []), c.id]);
        }
      }
      for (const [id] of ROOM_ROWS) {
        for (const k of keysFor(row(id), preset)) {
          // (a) nobody else binds this exact key.
          expect(byKey.get(k), `${preset} ${k}`).toEqual([id]);
          const toks = k.split(" ");
          // (b) no COMPLETE binding is a strict prefix of this chord.
          for (let i = 1; i < toks.length; i += 1) {
            const prefix = toks.slice(0, i).join(" ");
            expect(byKey.has(prefix), `${preset}: ${k} is shadowed by ${prefix}`).toBe(false);
          }
          // (c) this key is not a strict prefix of anyone ELSE's chord.
          for (const other of byKey.keys()) {
            if (other !== k) {
              expect(other.startsWith(`${k} `), `${preset}: ${k} would shadow ${other}`).toBe(false);
            }
          }
        }
      }
    }
  });

  it("resolves to its own id in review scope", () => {
    for (const [id, key] of ROOM_ROWS) {
      const hit = resolve(key, "review", {}, "vim");
      expect(hit?.id, `${key} should resolve to ${id}`).toBe(id);
    }
  });

  it("registers a handler for every row it ships, in the ONE route that owns them", () => {
    expect(REVIEW_DETAIL).toContain("useCommandHandlers");
    for (const [id] of ROOM_ROWS) {
      expect(REVIEW_DETAIL, `${id} has no handler in ReviewDetail.tsx`).toContain(`"${id}":`);
    }
  });

  it("names a real CLI twin or an honest none:<reason>", () => {
    for (const [id] of ROOM_ROWS) {
      const cli = row(id).cli ?? "";
      if (cli.startsWith("none:")) {
        expect(cli.slice(5).trim().length, `${id}: none: with no reason`).toBeGreaterThanOrEqual(4);
      } else {
        expect(cli.startsWith("kb-code "), `${id}: ${cli} is not a kb-code verb`).toBe(true);
      }
    }
  });
});
