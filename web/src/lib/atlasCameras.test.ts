import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  deleteCamera,
  listCameras,
  recallCamera,
  saveCamera,
  type AtlasCamera,
} from "./atlasCameras";

// vitest runs in the node env (no DOM) — stub a minimal in-memory
// localStorage for the persistence round-trip, same pattern as
// api/prefs.test.ts.
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

describe("atlasCameras", () => {
  beforeEach(() => stubLocalStorage());

  it("starts empty for a kb with no saved cameras", () => {
    expect(listCameras("canon")).toEqual([]);
  });

  it("save/list round-trips a camera", () => {
    const updated = saveCamera("canon", {
      name: "overview",
      pan: { x: 12, y: -4 },
      zoom: 1.6,
      colorMode: "clusters",
    });
    expect(updated).toHaveLength(1);
    expect(listCameras("canon")).toEqual([
      expect.objectContaining({
        v: 1,
        name: "overview",
        pan: { x: 12, y: -4 },
        zoom: 1.6,
        colorMode: "clusters",
      }),
    ]);
    expect(listCameras("canon")[0].savedAt).toEqual(expect.any(Number));
  });

  it("re-saving the same name overwrites rather than duplicating", () => {
    saveCamera("canon", { name: "overview", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters" });
    saveCamera("canon", { name: "overview", pan: { x: 5, y: 5 }, zoom: 2, colorMode: "clusters" });
    const all = listCameras("canon");
    expect(all).toHaveLength(1);
    expect(all[0].pan).toEqual({ x: 5, y: 5 });
    expect(all[0].zoom).toBe(2);
  });

  it("recall finds a saved camera by exact name", () => {
    saveCamera("canon", { name: "wide", pan: { x: 1, y: 2 }, zoom: 0.5, colorMode: "clusters" });
    expect(recallCamera("canon", "wide")?.zoom).toBe(0.5);
    expect(recallCamera("canon", "missing")).toBeUndefined();
  });

  it("delete removes a camera and is a no-op on an unknown name", () => {
    saveCamera("canon", { name: "a", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters" });
    saveCamera("canon", { name: "b", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters" });
    const afterDelete = deleteCamera("canon", "a");
    expect(afterDelete.map((c) => c.name)).toEqual(["b"]);
    expect(deleteCamera("canon", "nope").map((c) => c.name)).toEqual(["b"]);
  });

  it("cameras are namespaced per kb", () => {
    saveCamera("canon", { name: "shared-name", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters" });
    saveCamera("other", { name: "shared-name", pan: { x: 9, y: 9 }, zoom: 3, colorMode: "clusters" });
    expect(listCameras("canon")[0].pan).toEqual({ x: 0, y: 0 });
    expect(listCameras("other")[0].pan).toEqual({ x: 9, y: 9 });
  });

  it("corrupt JSON in storage yields an empty list instead of throwing", () => {
    localStorage.setItem("kb:atlas:cameras:canon", "{not json");
    expect(() => listCameras("canon")).not.toThrow();
    expect(listCameras("canon")).toEqual([]);
  });

  it("a non-array blob yields an empty list", () => {
    localStorage.setItem("kb:atlas:cameras:canon", JSON.stringify({ v: 1 }));
    expect(listCameras("canon")).toEqual([]);
  });

  it("drops malformed entries without discarding valid siblings", () => {
    const good: AtlasCamera = {
      v: 1,
      name: "good",
      pan: { x: 1, y: 1 },
      zoom: 1,
      colorMode: "clusters",
      savedAt: 1000,
    };
    const raw = [
      good,
      { v: 2, name: "future-schema" }, // wrong version
      { v: 1, name: "bad-colormode", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "read-state", savedAt: 1 },
      { v: 1, name: "", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters", savedAt: 1 }, // empty name
      { v: 1, name: "nan-zoom", pan: { x: 0, y: 0 }, zoom: Number.NaN, colorMode: "clusters", savedAt: 1 },
      "not-even-an-object",
      null,
    ];
    localStorage.setItem("kb:atlas:cameras:canon", JSON.stringify(raw));
    expect(listCameras("canon")).toEqual([good]);
  });

  it("MI-W4.5 — accepts the salience/decay color modes (rejects a still-unknown one)", () => {
    saveCamera("canon", { name: "sal", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "salience" });
    saveCamera("canon", { name: "dec", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "decay" });
    const raw = [
      { v: 1, name: "sal", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "salience", savedAt: 1 },
      { v: 1, name: "dec", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "decay", savedAt: 1 },
      { v: 1, name: "unknown", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "read-state", savedAt: 1 },
    ];
    localStorage.setItem("kb:atlas:cameras:canon", JSON.stringify(raw));
    const stored = listCameras("canon");
    expect(stored.map((c) => c.name)).toEqual(["sal", "dec"]);
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
    expect(() => listCameras("canon")).not.toThrow();
    expect(listCameras("canon")).toEqual([]);
    // save() must not throw even though persistence silently fails.
    expect(() =>
      saveCamera("canon", { name: "x", pan: { x: 0, y: 0 }, zoom: 1, colorMode: "clusters" }),
    ).not.toThrow();
  });
});
