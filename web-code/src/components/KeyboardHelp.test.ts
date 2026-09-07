import { describe, expect, it } from "vitest";
import { groupCommands, normaliseContext, scopeSections } from "./KeyboardHelp";
import { KBC_ACTIVE_COMMANDS } from "../commands/registry.gen";

// V70-A5 — the sheet's content is no longer a literal in this file, so this
// test no longer mirrors a private array. It covers the two pure functions
// that decide WHAT a scope's sheet shows, against the real registry: that is
// the whole point of generating the sheet — the test can finally assert about
// the actual bindings instead of a fixture that merely looks like them.

describe("normaliseContext", () => {
  it("maps the legacy spelling onto the real scope id", () => {
    expect(normaliseContext("review-diff")).toBe("diff");
    expect(normaliseContext("reader")).toBe("reader");
    expect(normaliseContext("all")).toBe("all");
  });
});

describe("groupCommands", () => {
  it("buckets by group, preserving registry order", () => {
    const reader = KBC_ACTIVE_COMMANDS.filter((c) => c.scope === "reader");
    const groups = groupCommands(reader);
    expect(groups.length).toBeGreaterThan(3);
    expect(groups.map((g) => g.title)).toContain("Move");
    expect(groups.map((g) => g.title)).toContain("Find");
    // Every command lands in exactly one bucket; none is lost.
    expect(groups.reduce((n, g) => n + g.rows.length, 0)).toBe(reader.length);
    // First-seen order, not alphabetical.
    expect(groups[0].title).toBe(reader[0].group);
  });
});

describe("scopeSections", () => {
  it("'all' returns every command as primary, nothing demoted", () => {
    const { primary, global } = scopeSections("all");
    expect(global).toEqual([]);
    // V73-K6 — `KBC_ACTIVE_COMMANDS`, not `KBC_COMMANDS`: a retired row
    // (`dismiss.sheet`) is excluded from the sheet on purpose.
    expect(primary.reduce((n, g) => n + g.rows.length, 0)).toBe(KBC_ACTIVE_COMMANDS.length);
  });

  it("a real scope shows its own rows first and collapses the global ones", () => {
    const { primary, global } = scopeSections("diff");
    expect(primary.flatMap((g) => g.rows).every((c) => c.scope === "diff")).toBe(true);
    expect(global.flatMap((g) => g.rows).every((c) => c.scope === "global")).toBe(true);
    expect(global.length).toBeGreaterThan(0);
    // The legacy context spelling lands on the same sheet.
    expect(scopeSections("review-diff").primary.map((g) => g.title)).toEqual(
      primary.map((g) => g.title),
    );
  });

  it("never renders an empty dialog", () => {
    for (const scope of ["rail", "drawer", "branches", "board", "palette"] as const) {
      expect(scopeSections(scope).primary.length, scope).toBeGreaterThan(0);
    }
  });

  it("lists rows that have NO key in a preset — the sheet is the registry, not the vim column", () => {
    const reader = scopeSections("reader").primary.flatMap((g) => g.rows);
    // `graph.ego` is bound in vim (`g G`) and unbound in plain; it must be
    // present either way, because the palette can still run it.
    expect(reader.map((c) => c.id)).toContain("graph.ego");
    expect(KBC_ACTIVE_COMMANDS.find((c) => c.id === "graph.ego")!.keys.plain).toEqual([]);
  });

  it("shows the ratified departures on the global sheet, marked planned", () => {
    const global = scopeSections("reader").global.flatMap((g) => g.rows);
    const planned = global.filter((c) => c.lifecycle !== "shipped").map((c) => c.id);
    // V71-E2 SHIPPED `action.panel` (`.` + Shift-F10 + ContextMenu over
    // `kbc-actions/1`), so it is no longer one of the planned departures —
    // it is on the sheet as a live row instead. `hint.*`/`scope.edit` are
    // still declared-not-built.
    expect(global.map((c) => c.id)).toContain("action.panel");
    expect(planned).not.toContain("action.panel");
    expect(planned).toContain("hint.jump");
    expect(planned).toContain("scope.edit");
  });
});
