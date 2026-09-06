import { describe, it, expect, beforeEach, vi } from "vitest";
import { deleteMark, getMark, listMarks, saveMark, type Mark } from "./marks";

// vitest runs in the node env (no DOM) — stub a minimal in-memory
// localStorage for the persistence round-trip, same pattern as
// `atlasCameras.test.ts` / `api/prefs.test.ts`.
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

describe("marks", () => {
  beforeEach(() => stubLocalStorage());

  it("starts empty with nothing saved", () => {
    expect(listMarks()).toEqual([]);
  });

  it("save/list round-trips a mark", () => {
    const updated = saveMark("a", {
      kb: "canon",
      sourceRelative: "ideas/foo.html",
      title: "Foo",
      sec: "intro",
    });
    expect(updated).toHaveLength(1);
    expect(listMarks()).toEqual([
      expect.objectContaining({
        v: 1,
        letter: "a",
        kb: "canon",
        sourceRelative: "ideas/foo.html",
        title: "Foo",
        sec: "intro",
      }),
    ]);
    expect(listMarks()[0].savedAt).toEqual(expect.any(Number));
  });

  it("re-saving the same letter overwrites rather than duplicating", () => {
    saveMark("a", { kb: "canon", sourceRelative: "one.html", title: "One", sec: null });
    saveMark("a", { kb: "other", sourceRelative: "two.html", title: "Two", sec: "s2" });
    const all = listMarks();
    expect(all).toHaveLength(1);
    expect(all[0].kb).toBe("other");
    expect(all[0].sourceRelative).toBe("two.html");
  });

  it("marks are cross-kb — a single global map, not namespaced per kb", () => {
    saveMark("a", { kb: "canon", sourceRelative: "one.html", title: "One", sec: null });
    saveMark("b", { kb: "other-kb", sourceRelative: "two.html", title: "Two", sec: null });
    const all = listMarks();
    expect(all.map((m) => m.kb).sort()).toEqual(["canon", "other-kb"]);
  });

  it("getMark finds by exact letter, case-insensitively", () => {
    saveMark("z", { kb: "canon", sourceRelative: "z.html", title: "Z", sec: null });
    expect(getMark("z")?.sourceRelative).toBe("z.html");
    expect(getMark("Z")?.sourceRelative).toBe("z.html");
    expect(getMark("missing" /* multi-char, invalid */)).toBeUndefined();
    expect(getMark("q")).toBeUndefined();
  });

  it("delete removes a mark and is a no-op on an unset letter", () => {
    saveMark("a", { kb: "canon", sourceRelative: "a.html", title: "A", sec: null });
    saveMark("b", { kb: "canon", sourceRelative: "b.html", title: "B", sec: null });
    const afterDelete = deleteMark("a");
    expect(afterDelete.map((m) => m.letter)).toEqual(["b"]);
    expect(deleteMark("q").map((m) => m.letter)).toEqual(["b"]);
  });

  it("the 26-slot grammar: every a-z letter is a valid distinct slot", () => {
    const letters = "abcdefghijklmnopqrstuvwxyz".split("");
    for (const l of letters) {
      saveMark(l, { kb: "canon", sourceRelative: `${l}.html`, title: l, sec: null });
    }
    expect(listMarks()).toHaveLength(26);
    for (const l of letters) expect(getMark(l)?.sourceRelative).toBe(`${l}.html`);
  });

  it("rejects out-of-grammar letters (non a-z, multi-char, uppercase input normalizes instead of rejecting)", () => {
    expect(saveMark("1", { kb: "canon", sourceRelative: "x.html", title: "X", sec: null })).toEqual(
      [],
    );
    expect(saveMark("ab", { kb: "canon", sourceRelative: "x.html", title: "X", sec: null })).toEqual(
      [],
    );
    // Uppercase normalizes to the same slot as lowercase rather than being
    // rejected — a chord-captured `KeyboardEvent.key` may arrive shifted.
    const updated = saveMark("A", { kb: "canon", sourceRelative: "a.html", title: "A", sec: null });
    expect(updated).toEqual([expect.objectContaining({ letter: "a" })]);
  });

  it("corrupt JSON in storage yields an empty list instead of throwing", () => {
    localStorage.setItem("kb:marks", "{not json");
    expect(() => listMarks()).not.toThrow();
    expect(listMarks()).toEqual([]);
  });

  it("a non-array blob yields an empty list", () => {
    localStorage.setItem("kb:marks", JSON.stringify({ v: 1 }));
    expect(listMarks()).toEqual([]);
  });

  it("drops malformed entries without discarding valid siblings", () => {
    const good: Mark = {
      v: 1,
      letter: "g",
      kb: "canon",
      sourceRelative: "good.html",
      title: "Good",
      sec: null,
      savedAt: 1000,
    };
    const raw = [
      good,
      { v: 2, letter: "x" }, // wrong version
      { v: 1, letter: "1", kb: "canon", sourceRelative: "a", title: "a", sec: null, savedAt: 1 }, // bad letter
      { v: 1, letter: "y", kb: "", sourceRelative: "a", title: "a", sec: null, savedAt: 1 }, // empty kb
      { v: 1, letter: "z", kb: "canon", sourceRelative: "a", title: "a", sec: 5, savedAt: 1 }, // bad sec type
      "not-even-an-object",
      null,
    ];
    localStorage.setItem("kb:marks", JSON.stringify(raw));
    expect(listMarks()).toEqual([good]);
  });

  it("localStorage.getItem throwing degrades to an empty list", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    });
    expect(() => listMarks()).not.toThrow();
    expect(listMarks()).toEqual([]);
    // save() must not throw even though persistence silently fails.
    expect(() =>
      saveMark("a", { kb: "canon", sourceRelative: "a.html", title: "A", sec: null }),
    ).not.toThrow();
  });
});
