import { describe, expect, it } from "vitest";
import { toView, type WhichKeyHint } from "./WhichKey";
import { pendingLabel } from "./dispatch";
import { KBC_ACTIVE_COMMANDS, KBC_LEADER } from "./registry.gen";

const [a, b, c] = KBC_ACTIVE_COMMANDS;
const hint = (command: typeof a, next: string, rest: string[]): WhichKeyHint => ({ command, next, rest });

describe("toView", () => {
  it("collapses to one row per next token; a lone single-token hint is a leaf, several are a counted group", () => {
    const v = toView(
      { pending: ["Space"], count: "" },
      [hint(a, "x", ["x"]), hint(b, "g", ["g", "d"]), hint(c, "g", ["g", "r"])],
      "reader",
    );
    expect(v.rows).toHaveLength(2);
    const x = v.rows.find((r) => r.keys[0] === "x")!;
    expect(x).toMatchObject({ verb: a.title, group: a.group, isGroup: false });
    const g = v.rows.find((r) => r.keys[0] === "g")!;
    expect(g).toMatchObject({ verb: "2 more…", group: b.group, isGroup: true });
    expect(v.scope).toBe("reader");
  });

  it("sorts by group then key", () => {
    const v = toView(
      { pending: ["g"], count: "" },
      KBC_ACTIVE_COMMANDS.slice(0, 40).map((cmd, i) => hint(cmd, String.fromCharCode(97 + (i % 26)), [String.fromCharCode(97 + (i % 26))])),
      "reader",
    );
    for (let i = 1; i < v.rows.length; i++) {
      const p = v.rows[i - 1];
      const q = v.rows[i];
      const ord = p.group === q.group ? p.keys[0].localeCompare(q.keys[0]) : p.group!.localeCompare(q.group!);
      expect(ord).toBeLessThanOrEqual(0);
    }
  });

  it("prefix joins to exactly what pendingLabel prints, leader glyph and count included", () => {
    for (const state of [
      { pending: ["Space", "g"], count: "" },
      { pending: ["g"], count: "12" },
      { pending: ["Space"], count: "" },
    ]) {
      expect(toView(state, [], "reader").prefix.join(" ")).toBe(pendingLabel(state));
    }
    expect(toView({ pending: ["Space"], count: "" }, [], "reader").prefix).toEqual([KBC_LEADER]);
  });
});
