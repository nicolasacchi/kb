import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { IDLE, keysFor, resolve, step, tokensOf } from "./dispatch";
import { KBC_COMMANDS, type KbcPreset } from "./registry.gen";

// V74-L3b — the five rows this unit adds, and the properties that make them
// safe. `deadRows.test.ts` and `surfaceRows.test.ts` already prove each has a
// real executor; this file proves the KEYS are the ones ratified and that they
// collide with nothing — the check `commands doctor` structurally cannot make
// for a leader PREFIX (V72-I2 and V73-K2c both shipped a collision it could
// not see).

const IDS = [
  "nav.tours",
  "tour.record",
  "tour.record-stop",
  "trail.pause",
  "rail.tab.trail",
] as const;

const PRESETS: KbcPreset[] = ["vim", "plain", "helix"];

function row(id: string) {
  const c = KBC_COMMANDS.find((x) => x.id === id);
  if (!c) throw new Error(`no registry row ${id}`);
  return c;
}

describe("V74-L3b registry rows", () => {
  it("every row exists, is shipped, and is dispatched centrally", () => {
    for (const id of IDS) {
      const c = row(id);
      expect(c.lifecycle, id).toBe("shipped");
      // All five are app-chrome commands the shell owns, not surface rows —
      // which is why `deadRows.test.ts` (central rows) is the gate for them.
      expect(c.dispatch, id).toBe("central");
      expect(c.scope, id).toBe("global");
    }
  });

  it("carries the RATIFIED keys, in all three presets", () => {
    const want: Record<string, string> = {
      "nav.tours": "Space g T",
      "tour.record": "Space k r",
      "tour.record-stop": "Space k s",
      "trail.pause": "Space k p",
      "rail.tab.trail": "Space R t",
    };
    for (const id of IDS) {
      for (const preset of PRESETS) {
        expect(keysFor(row(id), preset), `${id}/${preset}`).toEqual([want[id]]);
      }
    }
  });

  it("never binds `Space t` — that is `view.theme-cycle` in vim and helix", () => {
    for (const id of IDS) {
      for (const preset of PRESETS) {
        for (const k of keysFor(row(id), preset)) {
          expect(k.startsWith("Space t"), `${id}/${preset}: ${k}`).toBe(false);
        }
      }
    }
    expect(keysFor(row("view.theme-cycle"), "vim")).toEqual(["Space t"]);
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
          // (a) nobody else binds this exact key.
          expect(byKey.get(k), `${preset} ${k}`).toEqual([id]);
          const toks = k.split(" ");
          // (b) no COMPLETE binding is a strict prefix of this chord — that
          //     binding would fire first and this key would be unreachable.
          for (let i = 1; i < toks.length; i += 1) {
            const prefix = toks.slice(0, i).join(" ");
            expect(byKey.has(prefix), `${preset}: ${k} is shadowed by ${prefix}`).toBe(false);
          }
          // (c) this key is not a strict prefix of anyone ELSE's chord.
          for (const other of byKey.keys()) {
            if (other !== k) {
              expect(other.startsWith(`${k} `), `${preset}: ${k} would shadow ${other}`).toBe(
                false,
              );
            }
          }
        }
      }
    }
  });

  it("resolves to its own id from an empty global context", () => {
    for (const id of IDS) {
      const key = keysFor(row(id), "vim")[0];
      expect(resolve(key, "global", {}, "vim")?.id, id).toBe(id);
    }
  });

  it("walks the CHORD MACHINE token by token — every prefix is pending, the last is the match", () => {
    for (const id of IDS) {
      const key = keysFor(row(id), "vim")[0];
      const toks = tokensOf(key);
      let state = IDLE;
      let matched: string | null = null;
      for (const [i, t] of toks.entries()) {
        const out = step(state, t, "global", {}, "vim");
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
  // The same mechanical question `deadRows.test.ts` asks, narrowed to this
  // unit's rows and pinned to the FILE that owns each — so a later refactor
  // that moves a handler somewhere the key cannot reach fails here by name
  // rather than passing the "registered anywhere" check.
  const OWNERS: Record<string, string> = {
    "nav.tours": "app.tsx",
    "tour.record": "components/tours/TourRecorder.tsx",
    "tour.record-stop": "components/tours/TourRecorder.tsx",
    "trail.pause": "components/trail/TrailIndicator.tsx",
    "rail.tab.trail": "routes/Reader.tsx",
  };

  it("registers each row in the file that owns its surface", () => {
    for (const [id, rel] of Object.entries(OWNERS)) {
      const src = readFileSync(fileURLToPath(new URL(`../${rel}`, import.meta.url)), "utf8");
      expect(src.includes(`"${id}":`), `${id} in ${rel}`).toBe(true);
    }
  });
});
