import { describe, expect, it } from "vitest";
import {
  LABELS_KEY,
  MAX_LABEL_CHARS,
  MAX_PREFS,
  PINS_KEY,
  loadPrefs,
  prunePrefs,
  cleanLabel,
  setLabel,
  togglePin,
  type StorageLike,
} from "./termPrefs";

function mem(init: Record<string, string> = {}): StorageLike & {
  data: Map<string, string>;
  writes: number;
} {
  const data = new Map(Object.entries(init));
  const s = {
    data,
    writes: 0,
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => {
      s.writes++;
      data.set(k, v);
    },
    removeItem: (k: string) => {
      s.writes++;
      data.delete(k);
    },
  };
  return s;
}

const throwing: StorageLike = {
  getItem: () => {
    throw new Error("denied");
  },
  setItem: () => {
    throw new Error("quota");
  },
  removeItem: () => {
    throw new Error("denied");
  },
};

describe("labels", () => {
  it("sets, replaces and clears", () => {
    const s = mem();
    setLabel("a", "hello", s);
    expect(loadPrefs(s).labels).toEqual({ a: "hello" });
    setLabel("a", "world", s);
    expect(loadPrefs(s).labels).toEqual({ a: "world" });
    setLabel("a", null, s);
    expect(loadPrefs(s).labels).toEqual({});
    setLabel("a", "x", s);
    setLabel("a", "   ", s);
    expect(loadPrefs(s).labels).toEqual({});
  });
  it("trims, strips control characters and caps", () => {
    const s = mem();
    setLabel("a", "  hi\u0000 there\n\u009f  ", s);
    expect(loadPrefs(s).labels.a).toBe("hi there");
    setLabel("b", "é".repeat(100), s);
    expect(Array.from(loadPrefs(s).labels.b).length).toBe(MAX_LABEL_CHARS);
    setLabel("c", "😀".repeat(100), s);
    expect(Array.from(loadPrefs(s).labels.c).length).toBe(MAX_LABEL_CHARS);
  });
  it("rejects ids outside the grammar", () => {
    const s = mem();
    for (const id of ["", "A", "-a", "a b", "a/b", "x".repeat(33), "__proto__"]) setLabel(id, "x", s);
    expect(s.writes).toBe(0);
    expect(loadPrefs(s).labels).toEqual({});
  });
  it("evicts the oldest-inserted past MAX_PREFS, even for integer-like ids", () => {
    const s = mem();
    for (let i = 0; i < MAX_PREFS + 5; i++) setLabel(`${i + 10}`, `l${i}`, s);
    const l = loadPrefs(s).labels;
    expect(Object.keys(l).length).toBe(MAX_PREFS);
    expect(l["10"]).toBeUndefined();
    expect(l["14"]).toBeUndefined();
    expect(l["15"]).toBe("l5");
    expect(l[`${MAX_PREFS + 14}`]).toBe(`l${MAX_PREFS + 4}`);
  });
});

describe("pins", () => {
  it("toggle returns the new state", () => {
    const s = mem();
    expect(togglePin("a", s)).toBe(true);
    expect(loadPrefs(s).pins.has("a")).toBe(true);
    expect(togglePin("a", s)).toBe(false);
    expect(loadPrefs(s).pins.size).toBe(0);
    expect(togglePin("Bad Id", s)).toBe(false);
  });
  it("evicts the oldest past MAX_PREFS", () => {
    const s = mem();
    for (let i = 0; i < MAX_PREFS + 3; i++) togglePin(`p${i}`, s);
    const p = loadPrefs(s).pins;
    expect(p.size).toBe(MAX_PREFS);
    expect(p.has("p0")).toBe(false);
    expect(p.has("p3")).toBe(true);
  });
});

describe("prunePrefs", () => {
  it("removes stale ids, keeps live ones", () => {
    const s = mem();
    setLabel("a", "A", s);
    setLabel("b", "B", s);
    togglePin("a", s);
    togglePin("c", s);
    prunePrefs(["a"], s);
    const p = loadPrefs(s);
    expect(p.labels).toEqual({ a: "A" });
    expect([...p.pins]).toEqual(["a"]);
    prunePrefs([], s);
    expect(s.data.has(LABELS_KEY)).toBe(false);
    expect(s.data.has(PINS_KEY)).toBe(false);
  });
  it("does not write when nothing changed", () => {
    const s = mem();
    setLabel("a", "A", s);
    togglePin("a", s);
    const before = s.writes;
    prunePrefs(["a", "zzz"], s);
    expect(s.writes).toBe(before);
  });
  it("is literal: an empty list clears everything", () => {
    const s = mem();
    setLabel("a", "A", s);
    prunePrefs([], s);
    expect(loadPrefs(s).labels).toEqual({});
  });
});

describe("corruption and failure", () => {
  const bad = ["", "{", "null", "42", '"s"', '{"a":"b"}', "[1,2]", '[["a"]]', '[["A","x"]]', '[["a",5]]'];
  it("loadPrefs degrades every malformed value to empty", () => {
    for (const v of bad) {
      const s = mem({ [LABELS_KEY]: v, [PINS_KEY]: v });
      const p = loadPrefs(s);
      expect(p.labels).toEqual({});
      expect(p.pins.size).toBe(0);
    }
  });
  it("keeps valid entries around invalid ones", () => {
    const s = mem({ [LABELS_KEY]: '[["a","ok"],["B","no"],7,["c",""]]', [PINS_KEY]: '["a",3,"X","a"]' });
    const p = loadPrefs(s);
    expect(p.labels).toEqual({ a: "ok" });
    expect([...p.pins]).toEqual(["a"]);
  });
  it("overwrites a corrupt store on the next write", () => {
    const s = mem({ [LABELS_KEY]: "{" });
    setLabel("a", "x", s);
    expect(loadPrefs(s).labels).toEqual({ a: "x" });
  });
  it("throwing storage never throws", () => {
    expect(loadPrefs(throwing)).toEqual({ labels: {}, pins: new Set() });
    expect(() => setLabel("a", "x", throwing)).not.toThrow();
    expect(() => togglePin("a", throwing)).not.toThrow();
    expect(() => prunePrefs([], throwing)).not.toThrow();
  });
  it("no storage at all is empty prefs", () => {
    expect(loadPrefs(null)).toEqual({ labels: {}, pins: new Set() });
    expect(() => setLabel("a", "x", null)).not.toThrow();
  });
});

describe("cleanLabel invisibles", () => {
  it("strips zero-width and bidi characters so an all-invisible label is empty", () => {
    expect(cleanLabel("\u200B\u202E\u2066")).toBe("");
    expect(cleanLabel("a\u200Bb\u202Ec")).toBe("abc");
  });
});
