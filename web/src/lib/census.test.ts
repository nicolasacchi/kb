import { beforeEach, describe, expect, it } from "vitest";
import { censusBump, censusRead, censusReset } from "./census";

// vitest runs in a node environment — provide a minimal localStorage.
function installStorage() {
  const store = new Map<string, string>();
  (globalThis as { localStorage?: unknown }).localStorage = {
    getItem: (k: string) => store.get(k) ?? null,
    setItem: (k: string, v: string) => void store.set(k, v),
    removeItem: (k: string) => void store.delete(k),
  };
  return store;
}

describe("census (local-only counters)", () => {
  beforeEach(() => {
    installStorage();
  });

  it("starts empty and bumps counters", () => {
    expect(censusRead()).toEqual({});
    censusBump("atlas.open");
    censusBump("atlas.open");
    censusBump("resurface.click", 3);
    expect(censusRead()).toEqual({ "atlas.open": 2, "resurface.click": 3 });
  });

  it("survives corrupt storage by returning empty", () => {
    localStorage.setItem("kb:census", "{not json");
    expect(censusRead()).toEqual({});
    localStorage.setItem("kb:census", JSON.stringify([1, 2]));
    expect(censusRead()).toEqual({});
    localStorage.setItem("kb:census", JSON.stringify({ ok: 1, bad: "x" }));
    expect(censusRead()).toEqual({ ok: 1 });
  });

  it("reset clears everything", () => {
    censusBump("atlas.open");
    censusReset();
    expect(censusRead()).toEqual({});
  });

  it("is inert when storage is unavailable", () => {
    delete (globalThis as { localStorage?: unknown }).localStorage;
    expect(() => censusBump("atlas.open")).not.toThrow();
    expect(censusRead()).toEqual({});
  });
});
