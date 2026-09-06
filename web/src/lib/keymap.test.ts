import { describe, expect, test } from "vitest";
import {
  REGISTRY,
  groupRegistry,
  initialKeymapState,
  isEditableTarget,
  resetPending,
  resolve,
  type Binding,
} from "./keymap";

describe("chord resolution", () => {
  const gChords: [string, string][] = [
    ["a", "nav.atlas"],
    ["g", "nav.gallery"],
    ["m", "nav.memory"],
    ["l", "nav.lists"],
    ["n", "nav.notes"],
    ["s", "nav.sessions"],
    ["h", "nav.history"],
    [",", "nav.settings"],
    ["t", "nav.tocToggle"],
  ];

  test.each(gChords)("g %s resolves to %s", (second, actionId) => {
    const pending = resolve(initialKeymapState(), { key: "g" }, "global");
    expect(pending.kind).toBe("pending");
    expect(pending.state.pending).toEqual(["g"]);

    const matched = resolve(pending.state, { key: second }, "global");
    expect(matched.kind).toBe("matched");
    if (matched.kind !== "matched") throw new Error("unreachable");
    expect(matched.binding.actionId).toBe(actionId);
    // Resolving to a match always collapses back to idle.
    expect(matched.state).toEqual(initialKeymapState());
  });

  test("an unknown continuation fizzles and resets to idle", () => {
    const pending = resolve(initialKeymapState(), { key: "g" }, "global");
    expect(pending.kind).toBe("pending");
    const fizzled = resolve(pending.state, { key: "z" }, "global");
    expect(fizzled.kind).toBe("none");
    expect(fizzled.state).toEqual(initialKeymapState());
  });

  test("single-key global bindings resolve without a pending step", () => {
    const help = resolve(initialKeymapState(), { key: "?" }, "global");
    expect(help.kind).toBe("matched");
    if (help.kind !== "matched") throw new Error("unreachable");
    expect(help.binding.actionId).toBe("chrome.toggleHelp");

    const palette = resolve(initialKeymapState(), { key: "/" }, "global");
    expect(palette.kind).toBe("matched");
    if (palette.kind !== "matched") throw new Error("unreachable");
    expect(palette.binding.actionId).toBe("chrome.openPalette");
  });

  test("a key with no binding at all in the scope simply doesn't match", () => {
    const result = resolve(initialKeymapState(), { key: "q" }, "global");
    expect(result.kind).toBe("none");
    expect(result.state).toEqual(initialKeymapState());
  });

  test("Escape always wins, even mid-chord", () => {
    const pending = resolve(initialKeymapState(), { key: "g" }, "global");
    expect(pending.kind).toBe("pending");

    const esc = resolve(pending.state, { key: "Escape" }, "global");
    expect(esc.kind).toBe("matched");
    if (esc.kind !== "matched") throw new Error("unreachable");
    expect(esc.binding.actionId).toBe("chrome.escape");
    expect(esc.state).toEqual(initialKeymapState());
  });
});

describe("prefix timeout semantics", () => {
  test("resolve() never expires a pending chord on its own — the hook's timer must call resetPending", () => {
    const pending = resolve(initialKeymapState(), { key: "g" }, "global");
    expect(pending.kind).toBe("pending");
    // Nothing in this module expires `pending` — it stays live forever
    // until either a continuation key or an explicit reset arrives.
    expect(pending.state.pending).toEqual(["g"]);
  });

  test("resetPending collapses a pending chord back to idle (what the 800ms timeout calls)", () => {
    const pending = resolve(initialKeymapState(), { key: "g" }, "global");
    const afterTimeout = resetPending(pending.state);
    expect(afterTimeout).toEqual(initialKeymapState());

    // A key arriving after the reset starts a fresh sequence — bare "a"
    // isn't itself bound at global scope, so it fizzles rather than
    // completing "g a".
    const next = resolve(afterTimeout, { key: "a" }, "global");
    expect(next.kind).toBe("none");
  });
});

describe("W2.6b — hints/marks single-key globals resolve without a pending step", () => {
  test.each([
    ["f", "hint.open"],
    ["F", "hint.openTab"],
    ["m", "marks.enterSet"],
    ["`", "marks.enterJump"],
    // W3.P-c — registers use the SAME single-key-then-letter shape.
    ['"', "registers.enterSet"],
    ["'", "registers.enterPaste"],
  ] as const)("%s resolves to %s immediately (no chord ambiguity)", (key, actionId) => {
    const result = resolve(initialKeymapState(), { key }, "global");
    expect(result.kind).toBe("matched");
    if (result.kind !== "matched") throw new Error("unreachable");
    expect(result.binding.actionId).toBe(actionId);
    expect(result.state).toEqual(initialKeymapState());
  });

  // link-flow — `u` (back up the reading flow) joins the reader's doc-only
  // rows. Pinned here because the `?` sheet is the ONLY advertisement for
  // it, and because it must NOT disturb the reading-list trail's `n`/`p`.
  test("u is a doc-only reader binding and never resolves at scope global", () => {
    const row = REGISTRY.find((b) => b.actionId === "reader.flowBack");
    expect(row?.scope).toBe("reader");
    expect(row?.keys).toBe("u");
    expect(row?.group).toBe("Reader");
    expect(resolve(initialKeymapState(), { key: "u" }, "global").kind).toBe("none");
    // The trail queue's pair is untouched.
    expect(REGISTRY.find((b) => b.actionId === "lists.queueNext")?.keys).toBe("n");
    expect(REGISTRY.find((b) => b.actionId === "lists.queuePrev")?.keys).toBe("p");
  });

  test("y p is doc-only (reader scope) — resolve() never sees it at scope global", () => {
    const yankRow = REGISTRY.find((b) => b.actionId === "reader.yankProvenance");
    expect(yankRow?.scope).toBe("reader");
    // A bare "y" at global scope isn't bound to anything (detail.tsx owns
    // its own independent y-then-p tracking, outside this machine).
    const result = resolve(initialKeymapState(), { key: "y" }, "global");
    expect(result.kind).toBe("none");
  });
});

describe("scope precedence", () => {
  test("a scope-specific binding wins over a same-key global fallback", () => {
    const globalEscape = resolve(initialKeymapState(), { key: "Escape" }, "global");
    expect(globalEscape.kind).toBe("matched");
    if (globalEscape.kind !== "matched") throw new Error("unreachable");
    expect(globalEscape.binding.scope).toBe("global");

    const readerEscape = resolve(initialKeymapState(), { key: "Escape" }, "reader");
    expect(readerEscape.kind).toBe("matched");
    if (readerEscape.kind !== "matched") throw new Error("unreachable");
    expect(readerEscape.binding.scope).toBe("reader");
  });
});

describe("REGISTRY uniqueness — would have caught the two lying rows", () => {
  test("no duplicate (scope, keys, when) triple anywhere in the table", () => {
    const seen = new Set<string>();
    for (const b of REGISTRY) {
      const key = `${b.scope}|${b.keys}|${b.when ?? ""}`;
      expect(seen.has(key)).toBe(false);
      seen.add(key);
    }
  });

  test("global-scope keys are unique outright — they're all simultaneously live, so `when` can't split them", () => {
    const globalKeys = REGISTRY.filter((b) => b.scope === "global").map((b) => b.keys);
    expect(new Set(globalKeys).size).toBe(globalKeys.length);
  });

  test("every entry has a non-empty label and actionId (no placeholder rows)", () => {
    for (const b of REGISTRY) {
      expect(b.label.length).toBeGreaterThan(0);
      expect(b.actionId.length).toBeGreaterThan(0);
    }
  });
});

describe("groupRegistry", () => {
  test("covers every entry exactly once and preserves first-seen group order", () => {
    const groups = groupRegistry();
    const total = groups.reduce((n, g) => n + g.bindings.length, 0);
    expect(total).toBe(REGISTRY.length);
    expect(groups[0].group).toBe("Navigation");
    expect(groups.map((g) => g.group)).toEqual([...new Set(groups.map((g) => g.group))]);
    // W3.P-b — the pane family gets its own cheat-sheet section, and it sits
    // right after Reader (the table's iteration order IS the sheet's order).
    const names = groups.map((g) => g.group);
    expect(names).toContain("Panes");
    expect(names[names.indexOf("Panes") - 1]).toBe("Reader");
  });

  // W3.P-b — the `w` prefix family, pinned so a future edit can't silently
  // drop a row the `?` sheet is the only advertisement for.
  test("the Panes group is exactly the five reader-scope `w` chords", () => {
    const panes = groupRegistry().find((g) => g.group === "Panes");
    expect(panes).toBeTruthy();
    expect(panes!.bindings.map((b) => b.keys)).toEqual([
      "w v",
      "w q",
      "w h",
      "w l",
      "w o",
    ]);
    for (const b of panes!.bindings) {
      // NOT global: `useRovingCursor`'s independent window listener would
      // also see a global `w h`/`w l`. Doc-only, reader-scoped, owned by
      // routes/detail.tsx.
      expect(b.scope).toBe("reader");
      expect(b.when).toMatch(/^routes\/detail\.tsx/);
    }
  });

  // W3.R-c — the replay playhead family. Pinned like the Panes group above:
  // these rows are the `?` sheet's ONLY advertisement for them.
  test("the Replay group is exactly the four replay-scope playhead rows", () => {
    const replay = groupRegistry().find((g) => g.group === "Replay");
    expect(replay).toBeTruthy();
    expect(replay!.bindings.map((b) => b.keys)).toEqual([
      "← / →",
      "j / k",
      "[ / ]",
      "Home / End",
    ]);
    for (const b of replay!.bindings) {
      // NOT global: `useRovingCursor`'s independent window listener would
      // also see a global `j`/`k`. Doc-only, replay-scoped, owned by
      // routes/replay.tsx.
      expect(b.scope).toBe("replay");
      expect(b.when).toBe("routes/replay.tsx");
    }
  });

  test("no replay row is globally dispatched (j/k/[/] stay route-local)", () => {
    for (const key of ["j", "k", "[", "]"]) {
      expect(resolve(initialKeymapState(), { key }, "global").kind).toBe("none");
    }
  });

  test("the replay scope still inherits the global bindings (Escape, ?, g …)", () => {
    const help = resolve(initialKeymapState(), { key: "?" }, "replay");
    expect(help.kind).toBe("matched");
    if (help.kind !== "matched") throw new Error("unreachable");
    expect(help.binding.actionId).toBe("chrome.toggleHelp");
  });

  test("no `w` chord leaks into the globally-dispatched set", () => {
    // `resolve()` only ever runs at scope "global" (HotkeyRoot), so a `w`
    // entry landing there would start eating the key app-wide.
    const globalKeys = REGISTRY.filter((b) => b.scope === "global").map((b) => b.keys);
    expect(globalKeys.some((k) => k === "w" || k.startsWith("w "))).toBe(false);
    const typedW = resolve(initialKeymapState(), { key: "w" }, "global");
    expect(typedW.kind).toBe("none");
  });

  // W3.P-c — the `"`/`'` register family. The sheet is the ONLY
  // advertisement these two bindings have (no button, no menu item), so
  // the rows are pinned here the same way the Panes family is.
  test("the Registers group is exactly the two global letter-prefix bindings, and it follows Marks", () => {
    const groups = groupRegistry();
    const names = groups.map((g) => g.group);
    expect(names).toContain("Registers");
    expect(names[names.indexOf("Registers") - 1]).toBe("Marks");
    const regs = groups.find((g) => g.group === "Registers")!;
    expect(regs.bindings.map((b) => b.keys)).toEqual(['"', "'"]);
    for (const b of regs.bindings) {
      // Real, dispatched by HotkeyRoot — unlike Panes these are global,
      // because no sibling window listener claims a quote character.
      expect(b.scope).toBe("global");
      expect(b.actionId.startsWith("registers.")).toBe(true);
    }
  });

  test("registers and marks address one store, so the sheet advertises no second jump verb", () => {
    // The only register/mark actionIds in the table are the four
    // letter-prefix entries — nothing like a "registers.jump" that would
    // mean `'` and backtick both navigate.
    const ids = REGISTRY.filter(
      (b) => b.actionId.startsWith("marks.") || b.actionId.startsWith("registers."),
    ).map((b) => b.actionId);
    expect(ids.sort()).toEqual([
      "marks.enterJump",
      "marks.enterSet",
      "registers.enterPaste",
      "registers.enterSet",
    ]);
  });

  test("every binding in a group actually carries that group", () => {
    for (const { group, bindings } of groupRegistry()) {
      for (const b of bindings as Binding[]) expect(b.group).toBe(group);
    }
  });
});

describe("isEditableTarget", () => {
  const asTarget = (v: unknown) => v as EventTarget | null;

  test("flags INPUT/TEXTAREA/contentEditable; passes through everything else", () => {
    expect(isEditableTarget(null)).toBe(false);
    expect(isEditableTarget(asTarget({ tagName: "DIV" }))).toBe(false);
    expect(isEditableTarget(asTarget({ tagName: "INPUT" }))).toBe(true);
    expect(isEditableTarget(asTarget({ tagName: "TEXTAREA" }))).toBe(true);
    expect(isEditableTarget(asTarget({ tagName: "DIV", isContentEditable: true }))).toBe(true);
    expect(isEditableTarget(asTarget({ tagName: "BUTTON" }))).toBe(false);
  });
});
