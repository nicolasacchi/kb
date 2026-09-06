import { describe, expect, it } from "vitest";
import {
  initialVimKeyState,
  VIM_COMMAND_KINDS,
  vimKeysReducer,
  wordAt,
  type KeyInput,
  type VimCommand,
  type VimKeyState,
} from "./vimKeys";

/// Feed a sequence of keys through the reducer starting from `state`
/// (defaults to fresh), returning the final state and every command emitted
/// along the way (flattened, in order).
function run(
  keys: (string | KeyInput)[],
  state: VimKeyState = initialVimKeyState(),
): { state: VimKeyState; commands: VimCommand[] } {
  const commands: VimCommand[] = [];
  for (const k of keys) {
    const input: KeyInput = typeof k === "string" ? { key: k } : k;
    const result = vimKeysReducer(state, input);
    state = result.state;
    commands.push(...result.commands);
  }
  return { state, commands };
}

describe("counts", () => {
  it("5j moves down with count 5", () => {
    const { commands } = run(["5", "j"]);
    expect(commands).toEqual([{ t: "move", kind: "lineDown", count: 5 }]);
  });

  it("12G goes to line 12 (explicit count, not 'last line')", () => {
    const { commands } = run(["1", "2", "G"]);
    expect(commands).toEqual([{ t: "move", kind: "gotoBottom", count: 12 }]);
  });

  it("bare G (no count) means 'last line' — count is null, not 1", () => {
    const { commands } = run(["G"]);
    expect(commands).toEqual([{ t: "move", kind: "gotoBottom", count: null }]);
  });

  it("bare j (no count) carries count: null, not count: 1", () => {
    const { commands } = run(["j"]);
    expect(commands).toEqual([{ t: "move", kind: "lineDown", count: null }]);
  });

  it("count resets after being consumed by a motion", () => {
    const { state, commands } = run(["5", "j", "k"]);
    expect(commands).toEqual([
      { t: "move", kind: "lineDown", count: 5 },
      { t: "move", kind: "lineUp", count: null },
    ]);
    expect(state.pendingCount).toBe("");
  });

  it("count is discarded by an unrecognised key (no operator-pending leaks through)", () => {
    const { state, commands } = run(["5", "x"]);
    expect(commands).toEqual([]);
    expect(state.pendingCount).toBe("");
  });

  it("'d'-like keys are inert — this is a reader, there is no operator-pending mode", () => {
    // Plain 'd', 'c', 'p' have no meaning here (only Ctrl-d is bound, to
    // half-page-down) — pressing them alone must never start a pending
    // sequence or emit anything.
    // V70-A6 narrowed this list from `["d", "c", "p"]` to two: `p` is now
    // `cb-pin-pane` (pin a provisional pane — §P7's commitment ladder). It is
    // still not an OPERATOR, which is what this suite is really about: it
    // emits one callback command and settles, exactly like `a`/`Y`/`K`.
    for (const key of ["d", "c"]) {
      const { state, commands } = run([key]);
      expect(commands).toEqual([]);
      expect(state).toEqual(initialVimKeyState());
    }
    const pinned = run(["p"]);
    expect(pinned.commands).toEqual([{ t: "cb-pin-pane" }]);
    expect(pinned.state).toEqual(initialVimKeyState());
  });

  it("bare 'u' is the Ramp's Back rung (§P7) — NOT vim's undo (there is nothing to undo in a reader)", () => {
    const back = run(["u"]);
    expect(back.commands).toEqual([{ t: "cb-nav-back" }]);
    expect(back.state).toEqual(initialVimKeyState());
  });

  it("0 with no pending count is line-start", () => {
    const { commands } = run(["0"]);
    expect(commands).toEqual([{ t: "move", kind: "lineStart", count: null }]);
  });

  it("0 after a pending count joins the count (10, not line-start)", () => {
    const { commands } = run(["1", "0", "j"]);
    expect(commands).toEqual([{ t: "move", kind: "lineDown", count: 10 }]);
  });
});

describe("two-key sequences", () => {
  it("gg goes to the top (no count = line 1 via null)", () => {
    const { state, commands } = run(["g", "g"]);
    expect(commands).toEqual([{ t: "move", kind: "gotoTop", count: null }]);
    expect(state.pendingPrefix).toBe("");
  });

  it("Ngg goes to line N", () => {
    const { commands } = run(["7", "g", "g"]);
    expect(commands).toEqual([{ t: "move", kind: "gotoTop", count: 7 }]);
  });

  it("gd dispatches goto-def", () => {
    const { commands } = run(["g", "d"]);
    expect(commands).toEqual([{ t: "cb-goto-def" }]);
  });

  it("gr dispatches find-refs", () => {
    const { commands } = run(["g", "r"]);
    expect(commands).toEqual([{ t: "cb-find-refs" }]);
  });

  it("g. opens recent locations (V3.N1)", () => {
    const { commands } = run(["g", "."]);
    expect(commands).toEqual([{ t: "cb-recent-locations" }]);
  });

  it("gO opens structure popup (V3.N2)", () => {
    const { commands } = run(["g", "O"]);
    expect(commands).toEqual([{ t: "cb-structure-popup" }]);
  });

  it("gm toggles bookmark (V3.N2)", () => {
    const { commands } = run(["g", "m"]);
    expect(commands).toEqual([{ t: "cb-toggle-bookmark" }]);
  });

  it("gM opens mnemonic popup (V3.N2)", () => {
    const { commands } = run(["g", "M"]);
    expect(commands).toEqual([{ t: "cb-mnemonic-popup" }]);
  });

  it("gh opens line history (V3.R2 / R12)", () => {
    const { commands } = run(["g", "h"]);
    expect(commands).toEqual([{ t: "cb-line-history" }]);
  });

  it("gc opens callers hierarchy (V3.1-H3a)", () => {
    const { commands } = run(["g", "c"]);
    expect(commands).toEqual([{ t: "cb-hierarchy-callers" }]);
  });

  it("gC opens callees hierarchy (V3.1-H3a)", () => {
    const { commands } = run(["g", "C"]);
    expect(commands).toEqual([{ t: "cb-hierarchy-callees" }]);
  });

  it("gt opens type hierarchy (V3.1-H3a)", () => {
    const { commands } = run(["g", "t"]);
    expect(commands).toEqual([{ t: "cb-hierarchy-types" }]);
  });

  it("gi opens impact panel (V3.1-H3b)", () => {
    const { commands } = run(["g", "i"]);
    expect(commands).toEqual([{ t: "cb-impact" }]);
  });

  it("gG opens ego-graph (V3.1-H3b)", () => {
    const { commands } = run(["g", "G"]);
    expect(commands).toEqual([{ t: "cb-ego-graph" }]);
  });

  it("an unrecognised g-continuation cancels the prefix silently", () => {
    const { state, commands } = run(["g", "x"]);
    expect(commands).toEqual([]);
    expect(state.pendingPrefix).toBe("");
  });

  it("Ctrl-o / Ctrl-i dispatch jump back / forward (V3.N1)", () => {
    expect(run([{ key: "o", ctrl: true }]).commands).toEqual([{ t: "cb-jump-back" }]);
    expect(run([{ key: "i", ctrl: true }]).commands).toEqual([{ t: "cb-jump-forward" }]);
  });

  it("yy yanks the current line", () => {
    const { commands } = run(["y", "y"]);
    expect(commands).toEqual([{ t: "yank-line" }]);
  });

  it("a lone y starts the yy prefix and emits nothing yet", () => {
    const { state, commands } = run(["y"]);
    expect(commands).toEqual([]);
    expect(state.pendingPrefix).toBe("y");
  });

  it("m a sets mark 'a'", () => {
    const { commands } = run(["m", "a"]);
    expect(commands).toEqual([{ t: "mark-set", id: "a" }]);
  });

  it("' a jumps to mark 'a'", () => {
    const { commands } = run(["'", "a"]);
    expect(commands).toEqual([{ t: "mark-jump", id: "a" }]);
  });

  it("m a then ' a — a full set-then-jump round trip", () => {
    const { commands } = run(["m", "a", "'", "a"]);
    expect(commands).toEqual([{ t: "mark-set", id: "a" }, { t: "mark-jump", id: "a" }]);
  });

  it("[c / ]c step through history", () => {
    expect(run(["[", "c"]).commands).toEqual([{ t: "cb-history-step", dir: -1 }]);
    expect(run(["]", "c"]).commands).toEqual([{ t: "cb-history-step", dir: 1 }]);
  });

  it("[f / ]f cycle files", () => {
    expect(run(["[", "f"]).commands).toEqual([{ t: "cb-cycle-file", dir: -1 }]);
    expect(run(["]", "f"]).commands).toEqual([{ t: "cb-cycle-file", dir: 1 }]);
  });

  it("zz / zt / zb scroll the cursor line", () => {
    expect(run(["z", "z"]).commands).toEqual([{ t: "move", kind: "scrollCenter", count: null }]);
    expect(run(["z", "t"]).commands).toEqual([{ t: "move", kind: "scrollTop", count: null }]);
    expect(run(["z", "b"]).commands).toEqual([{ t: "move", kind: "scrollBottom", count: null }]);
  });

  it("Ctrl-w l focuses the next pane", () => {
    const { commands } = run([{ key: "w", ctrl: true }, { key: "l" }]);
    expect(commands).toEqual([{ t: "cb-pane-focus", dir: "next" }]);
  });

  it("Ctrl-w h focuses the previous pane", () => {
    const { commands } = run([{ key: "w", ctrl: true }, { key: "h" }]);
    expect(commands).toEqual([{ t: "cb-pane-focus", dir: "prev" }]);
  });

  it("Ctrl-w Ctrl-w also focuses the next pane", () => {
    const { commands } = run([{ key: "w", ctrl: true }, { key: "w", ctrl: true }]);
    expect(commands).toEqual([{ t: "cb-pane-focus", dir: "next" }]);
  });

  it("Ctrl-w v splits with self (opens the current file into pane2)", () => {
    const { commands, state } = run([{ key: "w", ctrl: true }, { key: "v" }]);
    expect(commands).toEqual([{ t: "cb-split-self" }]);
    expect(state.pendingPrefix).toBe("");
  });

  it("Ctrl-w q closes the focused pane", () => {
    const { commands, state } = run([{ key: "w", ctrl: true }, { key: "q" }]);
    expect(commands).toEqual([{ t: "cb-close-pane" }]);
    expect(state.pendingPrefix).toBe("");
  });

  it("an unrecognised Ctrl-w continuation cancels the prefix silently", () => {
    const { state, commands } = run([{ key: "w", ctrl: true }, { key: "x" }]);
    expect(commands).toEqual([]);
    expect(state.pendingPrefix).toBe("");
  });
});

describe("mode transitions", () => {
  it("normal -> visual -> normal via v/v", () => {
    let s = initialVimKeyState();
    let r = vimKeysReducer(s, { key: "v" });
    expect(r.state.mode).toBe("visual");
    expect(r.commands).toEqual([{ t: "mode-set", mode: "visual" }]);
    s = r.state;
    r = vimKeysReducer(s, { key: "v" });
    expect(r.state.mode).toBe("normal");
    expect(r.commands).toEqual([{ t: "mode-set", mode: "normal" }]);
  });

  it("V enters visual-line; V again exits to normal", () => {
    let s = initialVimKeyState();
    let r = vimKeysReducer(s, { key: "V" });
    expect(r.state.mode).toBe("visual-line");
    s = r.state;
    r = vimKeysReducer(s, { key: "V" });
    expect(r.state.mode).toBe("normal");
  });

  it("v while in visual-line switches to charwise visual (not back to normal)", () => {
    let s = initialVimKeyState();
    s = vimKeysReducer(s, { key: "V" }).state;
    expect(s.mode).toBe("visual-line");
    const r = vimKeysReducer(s, { key: "v" });
    expect(r.state.mode).toBe("visual");
  });

  it("Esc from visual collapses to normal", () => {
    let s = vimKeysReducer(initialVimKeyState(), { key: "v" }).state;
    const r = vimKeysReducer(s, { key: "Escape" });
    expect(r.state.mode).toBe("normal");
    expect(r.commands).toEqual([{ t: "mode-set", mode: "normal" }]);
  });

  it("Esc clears any pending count and prefix", () => {
    let s = vimKeysReducer(initialVimKeyState(), { key: "5" }).state;
    s = vimKeysReducer(s, { key: "g" }).state;
    expect(s.pendingCount).toBe("5");
    expect(s.pendingPrefix).toBe("g");
    const r = vimKeysReducer(s, { key: "Escape" });
    expect(r.state).toEqual({ mode: "normal", pendingCount: "", pendingPrefix: "" });
  });

  it("y in visual mode yanks the selection and exits to normal", () => {
    const s = vimKeysReducer(initialVimKeyState(), { key: "v" }).state;
    const r = vimKeysReducer(s, { key: "y" });
    expect(r.commands).toEqual([{ t: "yank-selection" }]);
    expect(r.state.mode).toBe("normal");
  });

  it("y in visual-line mode yanks the selection and exits to normal", () => {
    const s = vimKeysReducer(initialVimKeyState(), { key: "V" }).state;
    const r = vimKeysReducer(s, { key: "y" });
    expect(r.commands).toEqual([{ t: "yank-selection" }]);
    expect(r.state.mode).toBe("normal");
  });
});

describe("search + goto-line", () => {
  it("/ opens search", () => {
    expect(run(["/"]).commands).toEqual([{ t: "search-open" }]);
  });

  it(": opens goto-line", () => {
    expect(run([":"]).commands).toEqual([{ t: "goto-line-open" }]);
  });

  it("n / N step search forward/backward", () => {
    expect(run(["n"]).commands).toEqual([{ t: "search-step", dir: 1 }]);
    expect(run(["N"]).commands).toEqual([{ t: "search-step", dir: -1 }]);
  });

  it("* / # set the word-under-cursor query and step", () => {
    expect(run(["*"]).commands).toEqual([{ t: "search-word", dir: 1 }]);
    expect(run(["#"]).commands).toEqual([{ t: "search-word", dir: -1 }]);
  });
});

describe("action keys", () => {
  it("a / Y / K / ? dispatch their callbacks", () => {
    expect(run(["a"]).commands).toEqual([{ t: "cb-annotate" }]);
    expect(run(["Y"]).commands).toEqual([{ t: "cb-permalink" }]);
    expect(run(["K"]).commands).toEqual([{ t: "cb-hover" }]);
    expect(run(["?"]).commands).toEqual([{ t: "cb-show-help" }]);
  });
});

describe("Ctrl-modified scroll bindings", () => {
  it("Ctrl-d / Ctrl-u are half-page moves, Ctrl-f / Ctrl-b are full-page", () => {
    expect(run([{ key: "d", ctrl: true }]).commands).toEqual([
      { t: "move", kind: "halfPageDown", count: null },
    ]);
    expect(run([{ key: "u", ctrl: true }]).commands).toEqual([
      { t: "move", kind: "halfPageUp", count: null },
    ]);
    expect(run([{ key: "f", ctrl: true }]).commands).toEqual([
      { t: "move", kind: "fullPageDown", count: null },
    ]);
    expect(run([{ key: "b", ctrl: true }]).commands).toEqual([
      { t: "move", kind: "fullPageUp", count: null },
    ]);
  });

  it("plain b (no ctrl) is word-backward, not full-page-up", () => {
    expect(run(["b"]).commands).toEqual([{ t: "move", kind: "wordBackward", count: null }]);
  });
});

describe("wordAt", () => {
  it("finds the word when the cursor sits inside it", () => {
    expect(wordAt("const fooBar = 1;", 8)).toEqual({ word: "fooBar", start: 6, end: 12 });
  });

  it("finds the word when the cursor sits at its start", () => {
    expect(wordAt("const fooBar = 1;", 6)).toEqual({ word: "fooBar", start: 6, end: 12 });
  });

  it("falls back to the word just before the cursor when it's on punctuation/whitespace", () => {
    // Cursor at index 12 (the space right after "fooBar") — no word AT the
    // cursor, but one ending just before it.
    expect(wordAt("const fooBar = 1;", 12)).toEqual({ word: "fooBar", start: 6, end: 12 });
  });

  it("returns null between two words with no adjacent word char (e.g. ' = ')", () => {
    expect(wordAt("a  b", 2)).toBeNull();
  });

  it("handles the cursor at end-of-line, resting just after the last word", () => {
    expect(wordAt("return x", 8)).toEqual({ word: "x", start: 7, end: 8 });
  });

  it("handles the cursor at the very start of the line", () => {
    expect(wordAt("value", 0)).toEqual({ word: "value", start: 0, end: 5 });
  });

  it("treats underscores and digits as word characters", () => {
    expect(wordAt("my_var2 + 1", 3)).toEqual({ word: "my_var2", start: 0, end: 7 });
  });

  it("returns null on an empty line", () => {
    expect(wordAt("", 0)).toBeNull();
  });

  it("returns null when the cursor sits between two punctuation chars", () => {
    // Col 5 sits between the two dots of "..": index 4 ('.') and index 5
    // ('.') are both non-word, so there's no word on either side to fall
    // back to.
    expect(wordAt("foo .. bar", 5)).toBeNull();
  });
});

describe("no-edit guarantee", () => {
  // Every key this reducer recognises, plus junk it must ignore. If a new
  // command variant is ever added that CAN describe a document mutation,
  // this suite (the type-level `satisfies` in vimKeys.ts + the runtime
  // allowlist check below) is where it gets caught.
  const KEY_STORM: KeyInput[] = [
    ..."hjklwbe0^${}gGzZmnNy*#/:aYK?vV".split("").map((key) => ({ key })),
    { key: "g" }, { key: "g" }, // gg
    { key: "g" }, { key: "d" }, // gd
    { key: "g" }, { key: "r" }, // gr
    { key: "z" }, { key: "z" },
    { key: "z" }, { key: "t" },
    { key: "z" }, { key: "b" },
    { key: "[" }, { key: "c" },
    { key: "[" }, { key: "f" },
    { key: "]" }, { key: "c" },
    { key: "]" }, { key: "f" },
    { key: "m" }, { key: "a" },
    { key: "'" }, { key: "a" },
    { key: "y" }, { key: "y" },
    { key: "d", ctrl: true },
    { key: "u", ctrl: true },
    { key: "f", ctrl: true },
    { key: "b", ctrl: true },
    { key: "w", ctrl: true },
    { key: "h" },
    { key: "w", ctrl: true },
    { key: "w", ctrl: true },
    { key: "w", ctrl: true }, { key: "v" }, // Ctrl-w v (split with self)
    { key: "w", ctrl: true }, { key: "q" }, // Ctrl-w q (close pane)
    { key: "Escape" },
    // junk: unbound letters, digits, punctuation, stray ctrl combos
    ..."xcpqXPQZ!@%^&()_+=~".split("").map((key) => ({ key })),
    { key: "a", ctrl: true },
    { key: "c", ctrl: true },
    { key: "v", ctrl: true },
    { key: "z", ctrl: true },
    { key: "1" }, { key: "2" }, { key: "3" },
    { key: "F1" }, { key: "Tab" }, { key: "Enter" }, { key: "Backspace" },
  ];

  it("VIM_COMMAND_KINDS has no edit/change/insert/delete-shaped kind", () => {
    const banned = /insert|delete|replace|change|edit|write|mutate/i;
    for (const kind of VIM_COMMAND_KINDS) {
      expect(kind).not.toMatch(banned);
    }
  });

  it("a full keystroke storm never emits a command outside the allowlist", () => {
    let state = initialVimKeyState();
    const seen: VimCommand[] = [];
    for (const key of KEY_STORM) {
      const result = vimKeysReducer(state, key);
      state = result.state;
      seen.push(...result.commands);
    }
    expect(seen.length).toBeGreaterThan(0); // sanity: the storm did produce commands
    for (const cmd of seen) {
      expect(VIM_COMMAND_KINDS).toContain(cmd.t);
    }
  });

  it("the storm never leaves a stuck pending prefix/count (every sequence terminates)", () => {
    let state = initialVimKeyState();
    for (const key of KEY_STORM) {
      state = vimKeysReducer(state, key).state;
    }
    // The storm ends on plain junk keys, which always settle back to "".
    expect(state.pendingPrefix).toBe("");
    expect(state.pendingCount).toBe("");
  });
});

describe("V70-A4 — the Desk's Ctrl-w region chords", () => {
  function feed(keys: { key: string; ctrl?: boolean }[]) {
    let st = initialVimKeyState();
    const out: VimCommand[] = [];
    for (const k of keys) {
      const r = vimKeysReducer(st, k);
      st = r.state;
      out.push(...r.commands);
    }
    return { state: st, commands: out };
  }

  it("Ctrl-w r opens the resize submode", () => {
    expect(feed([{ key: "w", ctrl: true }, { key: "r" }]).commands).toEqual([{ t: "cb-resize-mode" }]);
  });

  it("Ctrl-w m zooms the focused region", () => {
    expect(feed([{ key: "w", ctrl: true }, { key: "m" }]).commands).toEqual([{ t: "cb-zoom-region" }]);
  });

  it("a bare m still starts the mark prefix — the chord only claims m AFTER Ctrl-w", () => {
    expect(feed([{ key: "m" }]).state.pendingPrefix).toBe("m");
    expect(feed([{ key: "m" }, { key: "a" }]).commands).toEqual([{ t: "mark-set", id: "a" }]);
  });

  it("both chords settle the prefix, so the next key starts fresh", () => {
    expect(feed([{ key: "w", ctrl: true }, { key: "r" }]).state.pendingPrefix).toBe("");
    expect(feed([{ key: "w", ctrl: true }, { key: "m" }]).state.pendingPrefix).toBe("");
  });
});
