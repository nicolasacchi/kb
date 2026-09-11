// V76-R2b — the review file-tree rows' keys, plus the prefix scan
// `commands doctor` cannot make for a leader PREFIX.
import { describe, expect, it } from "vitest";
import { IDLE, keysFor, resolve, step, tokensOf } from "./dispatch";
import { KBC_COMMANDS, type KbcPreset } from "./registry.gen";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

const IDS = [
  "diff.tree-focus",
  "diff.tree-toggle-folder",
  "diff.tree-collapse-all",
  "diff.map-reset",
] as const;

const PRESETS: KbcPreset[] = ["vim", "plain", "helix"];

function row(id: string) {
  const c = KBC_COMMANDS.find((x) => x.id === id);
  if (!c) throw new Error(`no registry row ${id}`);
  return c;
}

const REVIEW_DIFF_SRC = readFileSync(
  fileURLToPath(new URL("../routes/ReviewDiff.tsx", import.meta.url)),
  "utf-8",
);

describe("V76-R2b review file-tree registry rows", () => {
  it("every row exists, is shipped, diff-scoped and surface-dispatched", () => {
    for (const id of IDS) {
      const c = row(id);
      expect(c.lifecycle, id).toBe("shipped");
      expect(c.dispatch, id).toBe("surface");
      expect(c.scope, id).toBe("diff");
      expect(c.vimKind, id).toBeUndefined();
    }
  });

  it("carries the RATIFIED keys, in all three presets", () => {
    const want: Record<string, string> = {
      "diff.tree-focus": "g f",
      "diff.tree-toggle-folder": "z f",
      "diff.tree-collapse-all": "z m",
      "diff.map-reset": "g =",
    };
    for (const id of IDS) {
      for (const preset of PRESETS) {
        expect(keysFor(row(id), preset), `${id}/${preset}`).toEqual([want[id]]);
      }
    }
  });

  it("each row has an executor registered in ReviewDiff.tsx", () => {
    expect(REVIEW_DIFF_SRC).toContain("useCommandHandlers");
    for (const id of IDS) {
      expect(REVIEW_DIFF_SRC.includes(`"${id}":`), `${id} has no handler`).toBe(true);
    }
  });

  it("collides with nothing — no shared key, and no leader PREFIX either way", () => {
    for (const preset of PRESETS) {
      const byKey = new Map<string, string[]>();
      for (const c of KBC_COMMANDS) {
        for (const k of keysFor(c, preset)) {
          byKey.set(k, [...(byKey.get(k) ?? []), c.id]);
        }
      }
      for (const id of IDS) {
        for (const k of keysFor(row(id), preset)) {
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

  it("walks the CHORD MACHINE token by token in diff scope", () => {
    for (const id of IDS) {
      const key = keysFor(row(id), "vim")[0];
      const toks = tokensOf(key);
      let state = IDLE;
      let matched: string | null = null;
      for (const [i, t] of toks.entries()) {
        const out = step(state, t, "diff", {}, "vim");
        if (i < toks.length - 1) {
          expect(out.kind, `${id}: token ${t} must be pending, not ${out.kind}`).toBe("pending");
        } else {
          expect(out.kind, `${id}: ${key} must MATCH`).toBe("matched");
          if (out.kind === "matched") matched = out.command.id;
        }
        state = out.state;
      }
      expect(matched, id).toBe(id);
      expect(resolve(key, "diff", {}, "vim")?.id).toBe(id);
    }
  });
});
