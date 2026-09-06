// V70-A5 — the drift golden for the checked-in generated command registry.
//
// `registry.gen.ts` is generated from
// `crates/kb-code-server/commands/registry.json`, which lives in the SERVER
// crate (the daemon serves it verbatim at `GET /api/commands` via
// `include_str!`, and the CLI embeds the SAME file, so `kb-code commands
// explain` and the SPA dispatcher can never disagree about what a key means).
// The SPA cannot import across the crate boundary at BUILD time — the Docker
// SPA stage copies only `web-code/` — so it reads a checked-in copy, and a
// checked-in copy can go stale.
//
// This test is why it cannot: it re-runs the exact generator in memory and
// asserts byte equality. Edit the registry without running
// `npm run gen:commands` and this fails, naming the file to regenerate.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { generateRegistryTs, type RawRegistry } from "./derive";
import {
  KBC_CMD_SCHEMA,
  KBC_COMMANDS,
  KBC_DEFAULT_PRESET,
  KBC_LEADER,
  KBC_PRESETS,
  KBC_SCOPES,
} from "./registry.gen";

const REGISTRY_PATH = fileURLToPath(
  new URL("../../../crates/kb-code-server/commands/registry.json", import.meta.url),
);
const TS_PATH = fileURLToPath(new URL("./registry.gen.ts", import.meta.url));

const registry = JSON.parse(readFileSync(REGISTRY_PATH, "utf8")) as RawRegistry;

describe("the checked-in generated command registry", () => {
  it("registry.gen.ts is byte-identical to a fresh generation", () => {
    expect(
      generateRegistryTs(registry),
      "stale — run `npm run gen:commands` in web-code/",
    ).toBe(readFileSync(TS_PATH, "utf8"));
  });

  it("exposes the same command list to the SPA as the registry holds", () => {
    expect(KBC_COMMANDS.map((c) => c.id)).toEqual(registry.commands.map((c) => c.id));
  });
});

describe("the registry itself", () => {
  it("declares the kbc-cmd/1 schema and Space as the leader", () => {
    expect(registry.schema).toBe("kbc-cmd/1");
    expect(KBC_CMD_SCHEMA).toBe("kbc-cmd/1");
    // D2: "Space is only the leader, globally." Any other leader would make
    // the Desk's Space chords and the rehearsal overlay two different keys.
    expect(KBC_LEADER).toBe("Space");
  });

  it("has unique, namespaced ids", () => {
    const ids = KBC_COMMANDS.map((c) => c.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const id of ids) {
      expect(id, `${id} is not namespaced`).toMatch(/^[a-z][a-z0-9-]*(\.[a-z0-9-]+)+$/);
    }
  });

  it("names only declared scopes", () => {
    const declared = new Set(KBC_SCOPES.map((s) => s.id));
    for (const c of KBC_COMMANDS) expect(declared, c.id).toContain(c.scope);
  });

  it("carries three explicit preset columns on every row — never inheritance", () => {
    for (const c of KBC_COMMANDS) {
      for (const p of KBC_PRESETS) {
        expect(Array.isArray(c.keys[p.id]), `${c.id}.${p.id}`).toBe(true);
      }
    }
    expect(KBC_PRESETS.filter((p) => p.isDefault).map((p) => p.id)).toEqual([KBC_DEFAULT_PRESET]);
    expect(KBC_DEFAULT_PRESET).toBe("vim");
  });

  it("keeps the scope coactivity matrix symmetric", () => {
    // The conflicts report is only as honest as this matrix: an asymmetric
    // row silently deletes one direction of every collision between the pair.
    for (const a of KBC_SCOPES) {
      for (const b of a.coactiveWith) {
        const other = KBC_SCOPES.find((s) => s.id === b);
        expect(other, `${a.id} names unknown scope ${b}`).toBeDefined();
        expect(other!.coactiveWith, `${b} does not list ${a.id} back`).toContain(a.id);
      }
    }
  });

  it("gives every Escape row a dismiss order, and never lets Esc navigate", () => {
    // P2: "Esc rows are a first-class key class with a dismiss order
    // (innermost first); Esc never navigates."
    const escRows = KBC_COMMANDS.filter((c) =>
      Object.values(c.keys).some((col) => col.includes("Escape")),
    );
    expect(escRows.length).toBeGreaterThan(3);
    for (const c of escRows) {
      expect(c.dismissOrder, `${c.id} has no dismiss_order`).toBeTypeOf("number");
      expect(c.id, `${c.id} — Esc must dismiss, never navigate`).not.toMatch(/^nav\./);
      expect(c.mutation, `${c.id} — a dismissal never mutates`).toBe("none");
    }
    // Innermost first: the popover inside the palette dismisses before the
    // palette, which dismisses before a mode.
    const order = (id: string) => KBC_COMMANDS.find((c) => c.id === id)!.dismissOrder!;
    expect(order("dismiss.popover")).toBeLessThan(order("dismiss.palette"));
    expect(order("dismiss.menu")).toBeLessThan(order("dismiss.overlay"));
    expect(order("dismiss.overlay")).toBeLessThan(order("dismiss.mode"));
  });

  it("declares a CLI twin or an honest `none:<reason>` for every row", () => {
    for (const c of KBC_COMMANDS) {
      if (c.cli.startsWith("none:")) {
        expect(c.cli.length, `${c.id} — a bare "none:" is not a reason`).toBeGreaterThan(8);
      } else {
        expect(c.cli, c.id).toMatch(/^kb-code /);
      }
    }
  });

  it("records a note on every row whose key MOVED in this unit", () => {
    // D2: "Review-diff verbs that collide with reader verbs move under the
    // leader." Each move is a documented-surface change; the registry is
    // where the receipt lives (MOVED.md is the prose version).
    const moved = KBC_COMMANDS.filter((c) => c.note?.includes("MOVED"));
    expect(moved.map((c) => c.id).sort()).toEqual([
      "board.pan",
      "diff.split-toggle",
      "diff.toggle-viewed",
      "diff.tour-next",
      "diff.viewed-advance",
      "player.autoplay",
    ]);
  });
});
