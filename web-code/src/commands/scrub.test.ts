import { describe, expect, it } from "vitest";
import { IDLE, keysFor, resolve, step, tokensOf, type Ctx } from "./dispatch";
import { KBC_COMMANDS, type KbcPreset } from "./registry.gen";

const IDS = ["reader.scrub-toggle", "reader.scrub-prev", "reader.scrub-next"] as const;
const PRESETS: KbcPreset[] = ["vim", "plain", "helix"];

function row(id: string) {
  const c = KBC_COMMANDS.find((x) => x.id === id);
  if (!c) throw new Error(`no registry row ${id}`);
  return c;
}

describe("V76-R3d registry rows", () => {
  it("every row exists, is shipped, and is dispatched centrally", () => {
    for (const id of IDS) {
      const c = row(id);
      expect(c.lifecycle, id).toBe("shipped");
      expect(c.dispatch, id).toBe("central");
    }
    expect(row("reader.scrub-toggle").scope).toBe("global");
    expect(row("reader.scrub-prev").scope).toBe("reader");
    expect(row("reader.scrub-next").scope).toBe("reader");
  });

  it("carries the RATIFIED keys, in all three presets", () => {
    const want: Record<string, string> = {
      "reader.scrub-toggle": "Space H",
      "reader.scrub-prev": "[ H",
      "reader.scrub-next": "] H",
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

  it("never binds a bare `[` or `]` — those are mixed prefixes", () => {
    for (const id of IDS) {
      for (const preset of PRESETS) {
        for (const k of keysFor(row(id), preset)) {
          expect(k === "[", `${id}/${preset}`).toBe(false);
          expect(k === "]", `${id}/${preset}`).toBe(false);
        }
      }
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

  it("toggle resolves from a reader center; prev/next only while the strip is open", () => {
    expect(resolve("Space H", "global", { center: "reader" }, "vim")?.id).toBe("reader.scrub-toggle");
    expect(resolve("Space H", "global", {}, "vim")).toBeNull();
    expect(resolve("[ H", "reader", { "reader.scrub": true }, "vim")?.id).toBe("reader.scrub-prev");
    expect(resolve("] H", "reader", { "reader.scrub": true }, "vim")?.id).toBe("reader.scrub-next");
    expect(resolve("[ H", "reader", {}, "vim")).toBeNull();
  });

  it("walks the CHORD MACHINE token by token", () => {
    const cases: Array<[string, string, Ctx]> = [
      ["reader.scrub-toggle", "Space H", { center: "reader" }],
      ["reader.scrub-prev", "[ H", { "reader.scrub": true }],
      ["reader.scrub-next", "] H", { "reader.scrub": true }],
    ];
    for (const [id, key, ctx] of cases) {
      const toks = tokensOf(key);
      let state = IDLE;
      const scope = id === "reader.scrub-toggle" ? "global" : "reader";
      for (const [i, t] of toks.entries()) {
        const out = step(state, t, scope, ctx, "vim");
        if (i < toks.length - 1) {
          expect(out.kind, `${id}: token ${t}`).toBe("pending");
          if (out.kind === "pending") state = out.state;
        } else {
          expect(out.kind, `${id}: ${key}`).toBe("matched");
          if (out.kind === "matched") expect(out.command.id).toBe(id);
        }
      }
    }
  });
});
