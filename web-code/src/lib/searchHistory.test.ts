import { describe, expect, it } from "vitest";
import {
  cliLineFor,
  clearHistory,
  deleteSaved,
  HISTORY_CAP,
  HISTORY_KEY,
  loadHistory,
  loadSaved,
  pushHistory,
  saveSearch,
  SAVED_KEY,
  type StorageLike,
} from "./searchHistory";

function mem(seed: Record<string, string> = {}): StorageLike {
  const map = new Map(Object.entries(seed));
  return {
    getItem: (k) => map.get(k) ?? null,
    setItem: (k, v) => {
      map.set(k, v);
    },
  };
}

describe("searchHistory — browser-local history and saved searches", () => {
  it("records newest-first and moves a repeat rather than duplicating it", () => {
    const s = mem();
    pushHistory(s, "a");
    pushHistory(s, "b");
    expect(pushHistory(s, "a")).toEqual(["a", "b"]);
  });

  it("never records a blank and caps the ring", () => {
    const s = mem();
    pushHistory(s, "   ");
    expect(loadHistory(s)).toEqual([]);
    for (let i = 0; i < HISTORY_CAP + 5; i++) pushHistory(s, `q${i}`);
    const h = loadHistory(s);
    expect(h).toHaveLength(HISTORY_CAP);
    expect(h[0]).toBe(`q${HISTORY_CAP + 4}`);
  });

  it("degrades to empty on a corrupt or wrong-shaped blob, never throws", () => {
    expect(loadHistory(mem({ [HISTORY_KEY]: "{not json" }))).toEqual([]);
    expect(loadHistory(mem({ [HISTORY_KEY]: '{"a":1}' }))).toEqual([]);
    expect(loadHistory(mem({ [HISTORY_KEY]: "[1, null, \"ok\"]" }))).toEqual(["ok"]);
    expect(loadSaved(mem({ [SAVED_KEY]: '[{"name":"n"}]' }))).toEqual([]);
  });

  it("clears history", () => {
    const s = mem();
    pushHistory(s, "a");
    expect(clearHistory(s)).toEqual([]);
  });

  it("replaces a same-named saved search in place, keeping list order", () => {
    const s = mem();
    saveSearch(s, "first", "a");
    saveSearch(s, "second", "b");
    const next = saveSearch(s, "first", "a2");
    expect(next).toEqual([
      { name: "first", query: "a2" },
      { name: "second", query: "b" },
    ]);
    expect(deleteSaved(s, "first")).toEqual([{ name: "second", query: "b" }]);
  });

  it("refuses a blank name or a blank query rather than writing an unusable row", () => {
    const s = mem();
    expect(saveSearch(s, "  ", "q")).toEqual([]);
    expect(saveSearch(s, "n", "  ")).toEqual([]);
  });

  /// The CLI parity affordance IS the saved-search story on this side (see
  /// the module doc's recorded cut), so its quoting has to survive a query
  /// with a quote in it.
  it("builds a pasteable CLI line, quote-safe", () => {
    expect(cliLineFor("order lang:ruby")).toBe("kb-code search 'order lang:ruby'");
    expect(cliLineFor("order", "kb")).toBe("kb-code search 'order' --repo kb");
    expect(cliLineFor(`it's`)).toBe(`kb-code search 'it'\\''s'`);
  });
});
