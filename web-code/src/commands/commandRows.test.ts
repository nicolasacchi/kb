import { describe, expect, it } from "vitest";
import {
  commandNeedle,
  commandRows,
  COMMAND_PREFIX,
  deepLinkDisposition,
  isCommandQuery,
  splitRows,
} from "./commandRows";

describe("command mode", () => {
  it("recognises the `>` prefix and strips it", () => {
    expect(COMMAND_PREFIX).toBe(">");
    expect(isCommandQuery(">viewed")).toBe(true);
    expect(isCommandQuery("  > viewed")).toBe(true);
    expect(isCommandQuery("@symbol")).toBe(false);
    expect(commandNeedle("> go home")).toBe("go home");
    expect(commandNeedle(">")).toBe("");
    // Outside command mode the needle is still the trimmed query, so a caller
    // that pre-fills the box does not have to care which mode it is in.
    expect(commandNeedle("  home ")).toBe("home");
  });
});

describe("commandRows", () => {
  it("finds a command by title, by synonym and by id", () => {
    const byTitle = commandRows("> go to home", "reader", {}, "vim");
    expect(byTitle[0].command.id).toBe("nav.home");
    // `aka` carries the words an operator actually types.
    const bySynonym = commandRows("> seen", "diff", {}, "vim");
    expect(bySynonym.map((r) => r.command.id)).toContain("diff.toggle-viewed");
    const byId = commandRows("> reader.goto-definition", "reader", {}, "vim");
    expect(byId[0].command.id).toBe("reader.goto-definition");
  });

  it("highlights only a TITLE match — an id/synonym hit claims no ranges", () => {
    const t = commandRows("> home", "reader", {}, "vim").find((r) => r.command.id === "nav.home")!;
    expect(t.ranges.length).toBeGreaterThan(0);
    const s = commandRows("> seen", "diff", {}, "vim").find(
      (r) => r.command.id === "diff.toggle-viewed",
    )!;
    expect(s.ranges).toEqual([]);
  });

  it("shows the key for the ACTIVE preset, and null where the column is empty", () => {
    const vim = commandRows("> definition", "reader", {}, "vim")[0];
    expect(vim.key).toBe("g d");
    const plain = commandRows("> definition", "reader", {}, "plain")[0];
    expect(plain.key).toBe("F12");
    const ego = commandRows("> ego", "reader", {}, "plain").find(
      (r) => r.command.id === "graph.ego",
    )!;
    expect(ego.key).toBeNull();
  });

  it("marks out-of-scope, gated and planned rows unavailable — with a reason", () => {
    const rows = commandRows("", "reader", {}, "vim");
    const { available, unavailable } = splitRows(rows);
    expect(available.length).toBeGreaterThan(0);
    expect(unavailable.length).toBeGreaterThan(0);
    for (const r of unavailable) expect(r.reason, r.command.id).not.toBe("");

    const viewed = rows.find((r) => r.command.id === "diff.toggle-viewed")!;
    expect(viewed.available).toBe(false);
    expect(viewed.reason).toContain("Diff");

    const planned = rows.find((r) => r.command.id === "scope.edit")!;
    expect(planned.available).toBe(false);

    const gated = commandRows("", "diff", {}, "vim").find(
      (r) => r.command.id === "diff.disposition.agree",
    )!;
    expect(gated.available).toBe(false);
    expect(gated.reason).toContain("diff.menu");
    // …and available once the context says the menu is up.
    const open = commandRows("", "diff", { "diff.menu": true }, "vim").find(
      (r) => r.command.id === "diff.disposition.agree",
    )!;
    expect(open.available).toBe(true);
    expect(open.reason).toBe("");
  });

  it("orders available rows before unavailable ones, deterministically", () => {
    const rows = commandRows("> next", "diff", {}, "vim");
    const firstUnavailable = rows.findIndex((r) => !r.available);
    expect(rows.slice(0, firstUnavailable).every((r) => r.available)).toBe(true);
    expect(rows.slice(firstUnavailable).every((r) => !r.available)).toBe(true);
    // Same input, same output — the palette must not reshuffle under a
    // re-render.
    expect(commandRows("> next", "diff", {}, "vim").map((r) => r.command.id)).toEqual(
      rows.map((r) => r.command.id),
    );
  });
});

describe("?cmd= deep links", () => {
  it("auto-executes only read-only, side-effect-free commands", () => {
    expect(deepLinkDisposition("nav.home")).toMatchObject({ kind: "run" });
    expect(deepLinkDisposition("structure.open")).toMatchObject({ kind: "run" });
  });

  it("pre-fills the palette for anything that writes, or is not yet wired", () => {
    const write = deepLinkDisposition("bookmark.toggle");
    expect(write?.kind).toBe("prefill");
    expect(write?.kind === "prefill" ? write.reason : "").toContain("metadata");

    const planned = deepLinkDisposition("scope.edit");
    expect(planned?.kind).toBe("prefill");
    expect(planned?.kind === "prefill" ? planned.reason : "").toContain("planned");
  });

  it("returns null for an id the registry does not know", () => {
    expect(deepLinkDisposition("nope.nope")).toBeNull();
  });
});
