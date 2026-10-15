import { describe, expect, test } from "vitest";
import { REGISTRY, toKeyRows, toKeyView } from "./keymap";

describe("toKeyView (which-key adapter)", () => {
  test("prefix g lists every g continuation with its label", () => {
    const view = toKeyView(["g"], REGISTRY, "global")!;
    const byKey = Object.fromEntries(view.rows.map((r) => [r.keys[0], r.verb]));
    expect(byKey).toMatchObject({
      g: "Gallery (recent)",
      a: "Atlas",
      m: "Memory",
      l: "Reading lists",
      n: "Notes",
      s: "Sessions",
      b: "Slates (the board)",
      h: "History",
      t: "Topics rail: expand / shrink",
    });
    expect(view.rows.every((r) => !r.isGroup)).toBe(true);
    expect(view.prefix).toEqual(["g"]);
    expect(view.scope).toBe("Navigation");
  });

  test("marks and registers collapse to ONE slot row, not 26", () => {
    for (const p of ["m", "`", '"', "'"]) {
      const view = toKeyView([p], REGISTRY, "global")!;
      expect(view.rows).toHaveLength(1);
      expect(view.rows[0].keys).toEqual(["a–z"]);
    }
    expect(toKeyView(["m"], REGISTRY, "global")!.rows[0].verb).toBe("mark slot");
  });

  test("a prefix nothing extends yields null", () => {
    expect(toKeyView(["z"], REGISTRY, "global")).toBeNull();
    expect(toKeyView([], REGISTRY, "global")).toBeNull();
  });

  test("a next token leading to several bindings is a group row", () => {
    const reg = [
      { keys: "x a", scope: "global", group: "G", label: "A", actionId: "a" },
      { keys: "x b c", scope: "global", group: "G", label: "BC", actionId: "b" },
      { keys: "x b d", scope: "global", group: "G", label: "BD", actionId: "c" },
    ] as const;
    const rows = toKeyView(["x"], reg, "global")!.rows;
    expect(rows.find((r) => r.keys[0] === "a")).toMatchObject({ verb: "A", isGroup: false });
    expect(rows.find((r) => r.keys[0] === "b")).toMatchObject({ verb: "2 more…", isGroup: true });
  });
});

describe("toKeyRows (help-sheet adapter)", () => {
  test("covers every binding; keys join back to the registry string", () => {
    const rows = toKeyRows(REGISTRY);
    expect(rows).toHaveLength(REGISTRY.length);
    rows.forEach((r, i) => expect(r.keys.join(" ")).toBe(REGISTRY[i].keys));
  });

  test("lists a gallery and a reader section", () => {
    const groups = new Set(toKeyRows(REGISTRY).map((r) => r.group));
    expect(groups.has("Gallery")).toBe(true);
    expect(groups.has("Reader")).toBe(true);
  });

  test("a scope filter keeps that scope plus global", () => {
    const scopes = new Set(toKeyRows(REGISTRY, "reader").map((r) => r.scope));
    expect([...scopes].sort()).toEqual(["global", "reader"]);
  });
});
