import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  clearRegister,
  getRegister,
  listRegisters,
  registerContext,
  registerLabel,
  setRegister,
  type Ref,
  type Register,
} from "./registers";
import { getMark, listMarks, saveMark } from "./marks";

// vitest runs in the node env (no DOM) — stub a minimal in-memory
// localStorage, same pattern as `marks.test.ts` / `atlasCameras.test.ts`.
function stubLocalStorage() {
  const store: Record<string, string> = {};
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (k in store ? store[k] : null),
    setItem: (k: string, v: string) => {
      store[k] = v;
    },
    removeItem: (k: string) => {
      delete store[k];
    },
    clear: () => {
      for (const k of Object.keys(store)) delete store[k];
    },
  });
  return store;
}

const ARTIFACT: Ref = {
  kind: "artifact",
  kb: "canon",
  sourceRelative: "ideas/foo.html",
  title: "Foo",
  sec: "intro",
  id: "abc123",
};
const SELECTION: Ref = {
  kind: "selection",
  kb: "canon",
  sourceRelative: "ideas/foo.html",
  title: "Foo",
  sec: null,
  id: "abc123",
  cssPath: "main > p:nth-of-type(2)",
  offset: 12,
  snippet: "a quoted phrase",
};
const SESSION: Ref = { kind: "session", sessionId: "sess-0123456789ab", title: "session sess-0123456" };
const COMMIT: Ref = { kind: "commit", sha: "0f722cb4deadbeef", subject: "feat: the thing", repo: "kb" };

describe("registers — the store", () => {
  beforeEach(() => stubLocalStorage());

  it("starts empty", () => {
    expect(listRegisters()).toEqual([]);
  });

  it("set/get round-trips every kind of reference", () => {
    for (const [letter, ref] of [
      ["a", ARTIFACT],
      ["b", SELECTION],
      ["c", SESSION],
      ["d", COMMIT],
    ] as const) {
      setRegister(letter, ref);
    }
    expect(listRegisters()).toHaveLength(4);
    expect(getRegister("a")?.ref).toEqual(ARTIFACT);
    expect(getRegister("b")?.ref).toEqual(SELECTION);
    expect(getRegister("c")?.ref).toEqual(SESSION);
    expect(getRegister("d")?.ref).toEqual(COMMIT);
    expect(getRegister("a")?.savedAt).toEqual(expect.any(Number));
  });

  it("overwrites by letter rather than duplicating — one reference per slot", () => {
    setRegister("a", ARTIFACT);
    setRegister("a", SESSION);
    expect(listRegisters()).toHaveLength(1);
    expect(getRegister("a")?.ref.kind).toBe("session");
  });

  it("the hard 26-slot cap: every a–z letter is a distinct slot, and nothing else is a slot", () => {
    const letters = "abcdefghijklmnopqrstuvwxyz".split("");
    for (const l of letters) {
      setRegister(l, { ...(ARTIFACT as Extract<Ref, { kind: "artifact" }>), title: l });
    }
    expect(listRegisters()).toHaveLength(26);
    // Out-of-grammar writes are refused on the WRITE side, so they can't
    // wedge an unreachable 27th slot into storage.
    expect(setRegister("1", ARTIFACT)).toHaveLength(26);
    expect(setRegister("ab", ARTIFACT)).toHaveLength(26);
    expect(setRegister("", ARTIFACT)).toHaveLength(26);
    expect(listRegisters()).toHaveLength(26);
  });

  it("normalizes an uppercase (shifted) letter to the same slot instead of rejecting it", () => {
    setRegister("A", ARTIFACT);
    expect(listRegisters().map((r) => r.letter)).toEqual(["a"]);
    expect(getRegister("A")?.ref).toEqual(ARTIFACT);
  });

  it("refuses a malformed ref rather than storing an unreadable slot", () => {
    setRegister("a", ARTIFACT);
    // Wrong tag, missing fields, wrong field types — each a no-op.
    expect(setRegister("b", { kind: "nope" } as unknown as Ref)).toHaveLength(1);
    expect(setRegister("b", { kind: "session" } as unknown as Ref)).toHaveLength(1);
    expect(
      setRegister("b", { kind: "commit", sha: "", subject: "x" } as unknown as Ref),
    ).toHaveLength(1);
    expect(
      setRegister("b", { ...SELECTION, offset: "12" } as unknown as Ref),
    ).toHaveLength(1);
    expect(listRegisters().map((r) => r.letter)).toEqual(["a"]);
  });

  it("clear removes a slot and is a no-op on an unset one", () => {
    setRegister("a", ARTIFACT);
    setRegister("b", SESSION);
    expect(clearRegister("a").map((r) => r.letter)).toEqual(["b"]);
    expect(clearRegister("q").map((r) => r.letter)).toEqual(["b"]);
    expect(getRegister("a")).toBeUndefined();
  });

  it("getRegister rejects an out-of-grammar letter instead of scanning", () => {
    setRegister("a", ARTIFACT);
    expect(getRegister("1")).toBeUndefined();
    expect(getRegister("ab")).toBeUndefined();
  });
});

describe("registers — validated on read (a corrupt blob degrades, never throws)", () => {
  beforeEach(() => stubLocalStorage());

  it("corrupt JSON yields an empty list", () => {
    localStorage.setItem("kb:registers", "{not json");
    expect(() => listRegisters()).not.toThrow();
    expect(listRegisters()).toEqual([]);
  });

  it("a non-array blob yields an empty list", () => {
    localStorage.setItem("kb:registers", JSON.stringify({ v: 1 }));
    expect(listRegisters()).toEqual([]);
  });

  it("drops malformed entries without discarding valid siblings", () => {
    const good: Register = { v: 1, letter: "g", savedAt: 1000, ref: ARTIFACT };
    const raw = [
      good,
      { v: 2, letter: "x", savedAt: 1, ref: ARTIFACT }, // wrong version
      { v: 1, letter: "1", savedAt: 1, ref: ARTIFACT }, // bad letter
      { v: 1, letter: "y", savedAt: "nope", ref: ARTIFACT }, // bad savedAt
      { v: 1, letter: "z", savedAt: 1, ref: { kind: "artifact", kb: "" } }, // bad ref
      { v: 1, letter: "w", savedAt: 1 }, // no ref at all
      "not-even-an-object",
      null,
    ];
    localStorage.setItem("kb:registers", JSON.stringify(raw));
    expect(listRegisters()).toEqual([good]);
  });

  it("localStorage throwing degrades to an empty list and a silent write", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    });
    expect(() => listRegisters()).not.toThrow();
    expect(listRegisters()).toEqual([]);
    expect(() => setRegister("a", ARTIFACT)).not.toThrow();
    expect(() => clearRegister("a")).not.toThrow();
  });
});

describe("registers — the W2.6b marks migration (kb:marks → kb:registers)", () => {
  beforeEach(() => stubLocalStorage());

  const legacyMark = (letter: string, overrides: Record<string, unknown> = {}) => ({
    v: 1,
    letter,
    kb: "canon",
    sourceRelative: `${letter}.html`,
    title: letter.toUpperCase(),
    sec: null,
    savedAt: 5000,
    ...overrides,
  });

  it("folds old marks forward as artifact refs, preserving savedAt exactly", () => {
    localStorage.setItem(
      "kb:marks",
      JSON.stringify([legacyMark("a"), legacyMark("b", { sec: "intro" })]),
    );
    const regs = listRegisters();
    expect(regs).toHaveLength(2);
    expect(regs.map((r) => r.letter).sort()).toEqual(["a", "b"]);
    const a = regs.find((r) => r.letter === "a")!;
    expect(a.savedAt).toBe(5000); // NOT re-stamped "now"
    expect(a.ref).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "a.html",
      title: "A",
      sec: null,
      // A W2.6b mark never carried the artifact id.
      id: null,
    });
    expect(regs.find((r) => r.letter === "b")!.ref).toMatchObject({ sec: "intro" });
  });

  it("consumes the legacy key so a later clear can't be undone by a re-migration", () => {
    localStorage.setItem("kb:marks", JSON.stringify([legacyMark("a")]));
    expect(listRegisters()).toHaveLength(1);
    expect(localStorage.getItem("kb:marks")).toBeNull();
    expect(localStorage.getItem("kb:registers")).toBeTruthy();

    clearRegister("a");
    expect(listRegisters()).toEqual([]);
    // The regression this guards: a non-consuming migration would resurrect
    // the mark on the very next read.
    expect(listRegisters()).toEqual([]);
  });

  it("drops malformed legacy rows but carries their valid siblings", () => {
    localStorage.setItem(
      "kb:marks",
      JSON.stringify([
        legacyMark("a"),
        legacyMark("1"), // bad letter
        legacyMark("b", { kb: "" }), // empty kb
        legacyMark("c", { sec: 5 }), // bad sec type
        legacyMark("d", { v: 2 }), // wrong version
        "junk",
        null,
      ]),
    );
    expect(listRegisters().map((r) => r.letter)).toEqual(["a"]);
  });

  it("a corrupt legacy blob migrates to nothing and still clears the key", () => {
    localStorage.setItem("kb:marks", "{not json");
    expect(listRegisters()).toEqual([]);
    expect(localStorage.getItem("kb:marks")).toBeNull();
  });

  it("an existing register wins a same-letter collision with a stale mark", () => {
    setRegister("a", SESSION);
    localStorage.setItem("kb:marks", JSON.stringify([legacyMark("a"), legacyMark("b")]));
    const regs = listRegisters();
    expect(regs).toHaveLength(2);
    expect(getRegister("a")?.ref).toEqual(SESSION);
    expect(getRegister("b")?.ref).toMatchObject({ kind: "artifact", sourceRelative: "b.html" });
  });

  it("no legacy key at all = no migration write", () => {
    expect(listRegisters()).toEqual([]);
    expect(localStorage.getItem("kb:registers")).toBeNull();
  });
});

describe("marks are the artifact-kind lens on the SAME store (the subsumption)", () => {
  beforeEach(() => stubLocalStorage());

  it("a mark saved through the marks façade is readable as a register", () => {
    saveMark("a", { kb: "canon", sourceRelative: "x.html", title: "X", sec: "s1" });
    const reg = getRegister("a");
    expect(reg?.ref).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "x.html",
      title: "X",
      sec: "s1",
    });
    // ...and there is exactly one storage key, not two.
    expect(localStorage.getItem("kb:marks")).toBeNull();
    expect(localStorage.getItem("kb:registers")).toBeTruthy();
  });

  it("an artifact register stored by `\" a` is jumpable by backtick (it IS a mark)", () => {
    setRegister("a", ARTIFACT);
    expect(getMark("a")).toMatchObject({
      letter: "a",
      kb: "canon",
      sourceRelative: "ideas/foo.html",
      sec: "intro",
    });
  });

  it("a non-artifact register is NOT a mark — backtick has nowhere to jump", () => {
    setRegister("a", SESSION);
    setRegister("b", COMMIT);
    setRegister("c", ARTIFACT);
    expect(getMark("a")).toBeUndefined();
    expect(getMark("b")).toBeUndefined();
    expect(listMarks().map((m) => m.letter)).toEqual(["c"]);
    // The slots are still occupied — a mark lens just doesn't show them.
    expect(listRegisters()).toHaveLength(3);
  });

  it("a migrated W2.6b mark is still a mark after the subsumption", () => {
    localStorage.setItem(
      "kb:marks",
      JSON.stringify([
        { v: 1, letter: "z", kb: "canon", sourceRelative: "z.html", title: "Z", sec: null, savedAt: 7 },
      ]),
    );
    expect(getMark("z")).toEqual({
      v: 1,
      letter: "z",
      kb: "canon",
      sourceRelative: "z.html",
      title: "Z",
      sec: null,
      savedAt: 7,
    });
  });
});

describe("display helpers", () => {
  beforeEach(() => stubLocalStorage());

  it("labels each kind and truncates a long selection snippet", () => {
    expect(registerLabel(ARTIFACT)).toBe("Foo");
    expect(registerLabel(SELECTION)).toBe("“a quoted phrase”");
    expect(registerLabel(SESSION)).toBe("session sess-0123456");
    expect(registerLabel(COMMIT)).toBe("feat: the thing");
    const long = { ...(SELECTION as Extract<Ref, { kind: "selection" }>), snippet: "x".repeat(80) };
    expect(registerLabel(long)).toBe(`“${"x".repeat(40)}…”`);
  });

  it("falls back to the durable field when the human label is blank", () => {
    expect(registerLabel({ ...(ARTIFACT as Extract<Ref, { kind: "artifact" }>), title: "" })).toBe(
      "ideas/foo.html",
    );
    expect(registerLabel({ ...(SESSION as Extract<Ref, { kind: "session" }>), title: "" })).toBe(
      "sess-0123456789ab",
    );
    expect(registerLabel({ ...(COMMIT as Extract<Ref, { kind: "commit" }>), subject: "" })).toBe(
      "0f722cb4deadbeef",
    );
  });

  it("context is the corpus / the short id", () => {
    expect(registerContext(ARTIFACT)).toBe("canon");
    expect(registerContext(SELECTION)).toBe("canon");
    expect(registerContext(SESSION)).toBe("sess-0123456");
    expect(registerContext(COMMIT)).toBe("0f722cb4");
  });
});
