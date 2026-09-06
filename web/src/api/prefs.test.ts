import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  coldSeedKb,
  coldSeedSessionsView,
  coldSeedShell,
  loadHome,
  loadLastKb,
  loadLastSessionsView,
  loadPrefs,
  saveHome,
  saveLastKb,
  saveLastSessionsView,
  DEFAULT_PREFS,
} from "./prefs";

// vitest runs in the node env here (no DOM), so stub a minimal in-memory
// localStorage for the persistence round-trip.
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
}

describe("coldSeedKb (K1 cold-entry gating)", () => {
  const KBS = ["alpha", "beta", "gamma"]; // alpha is the kbs[0] default

  // invariant:33
  it("seeds the remembered kb on a bare cold entry", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/", lastKb: "beta" }),
    ).toBe("beta");
  });

  it("does nothing while the kb list is still loading", () => {
    expect(
      coldSeedKb({ kbs: [], explicitKb: null, pathname: "/", lastKb: "beta" }),
    ).toBeNull();
  });

  it("yields to an explicit selection", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: "gamma", pathname: "/", lastKb: "beta" }),
    ).toBeNull();
  });

  it("only seeds on the bare '/' path", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/notes", lastKb: "beta" }),
    ).toBeNull();
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/a/beta/x", lastKb: "beta" }),
    ).toBeNull();
  });

  it("no-ops when there is nothing remembered", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/", lastKb: null }),
    ).toBeNull();
  });

  it("no-ops when the remembered kb is already the kbs[0] default", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/", lastKb: "alpha" }),
    ).toBeNull();
  });

  it("ghost-kb guard: ignores a remembered kb no longer configured (#13)", () => {
    expect(
      coldSeedKb({ kbs: KBS, explicitKb: null, pathname: "/", lastKb: "deleted" }),
    ).toBeNull();
  });
});

// W3.M-d — the map-home evidence gate. These tests exist to keep the gate
// from quietly eroding: the DEFAULT must stay "grid" until the atlas census
// earns the promotion (criterion recorded in prefs.ts), and only a truly
// bare cold entry may be redirected to the map.
describe("home pref + coldSeedShell (M-d map-home gate)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults to the GRID — map-home is opt-in until the census fires", () => {
    expect(DEFAULT_PREFS.home).toBe("grid");
    expect(loadHome()).toBe("grid");
    expect(
      coldSeedShell({ home: loadHome(), pathname: "/", search: "" }),
    ).toBeNull();
  });

  it("an unknown / corrupt stored value degrades to the grid", () => {
    localStorage.setItem("kb:prefs", JSON.stringify({ home: "atlas-3d" }));
    expect(loadHome()).toBe("grid");
  });

  it("round-trips the deliberate opt-in", () => {
    saveHome("map");
    expect(loadHome()).toBe("map");
    const p = loadPrefs();
    // Rides the same blob as lastKb, and leaves the styling prefs alone.
    expect(p.theme).toBe("dark");
    saveHome("grid");
    expect(loadHome()).toBe("grid");
  });

  it("seeds the map only on a TRULY bare cold entry", () => {
    expect(coldSeedShell({ home: "map", pathname: "/", search: "" })).toBe("map");
    expect(coldSeedShell({ home: "map", pathname: "/", search: "?" })).toBe("map");
  });

  it("never hijacks a URL that already carries params or a shell flag", () => {
    for (const search of [
      "?kb=alpha",
      "?view=list",
      "?shell=map",
      "?shell=grid",
      "?tags=x",
    ]) {
      expect(coldSeedShell({ home: "map", pathname: "/", search })).toBeNull();
    }
  });

  it("only fires on the bare '/' path", () => {
    expect(
      coldSeedShell({ home: "map", pathname: "/notes", search: "" }),
    ).toBeNull();
    expect(
      coldSeedShell({ home: "map", pathname: "/a/canon/x.html", search: "" }),
    ).toBeNull();
  });
});

describe("lastKb persistence", () => {
  beforeEach(() => stubLocalStorage());

  it("round-trips through localStorage", () => {
    expect(loadLastKb()).toBeNull();
    saveLastKb("beta");
    expect(loadLastKb()).toBe("beta");
  });

  it("is kept out of the other prefs (theme/accent/density preserved)", () => {
    saveLastKb("beta");
    const p = loadPrefs();
    expect(p.lastKb).toBe("beta");
    // The styling prefs keep their defaults — lastKb rides the same blob but is
    // an independent field (and stays out of the patchSettings allow-list).
    expect(p.theme).toBe("dark");
    expect(p.accent).toBe("violet");
    expect(p.density).toBe("comfy");
  });
});

describe("lastSessionsView persistence (W3.C/D3)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults to list", () => {
    expect(loadLastSessionsView()).toBe("list");
  });

  it("round-trips through localStorage", () => {
    saveLastSessionsView("projects");
    expect(loadLastSessionsView()).toBe("projects");
    saveLastSessionsView("list");
    expect(loadLastSessionsView()).toBe("list");
  });

  it("degrades an unknown/hand-edited value to list", () => {
    localStorage.setItem(
      "kb:prefs",
      JSON.stringify({ ...DEFAULT_PREFS, lastSessionsView: "bogus" }),
    );
    expect(loadLastSessionsView()).toBe("list");
  });
});

describe("coldSeedSessionsView (W3.C/D3 cold-entry gating)", () => {
  it("promotes to projects only on a truly bare /sessions with the remembered pref set", () => {
    expect(
      coldSeedSessionsView({ search: "", lastSessionsView: "projects" }),
    ).toBe("projects");
    expect(
      coldSeedSessionsView({ search: "?", lastSessionsView: "projects" }),
    ).toBe("projects");
  });

  it("stays list-default when nothing is remembered", () => {
    expect(
      coldSeedSessionsView({ search: "", lastSessionsView: "list" }),
    ).toBeNull();
  });

  it("yields to ANY query string — a deep link always wins", () => {
    for (const search of ["?focus=sid-1", "?q=bug", "?project=kb", "?view=list"]) {
      expect(
        coldSeedSessionsView({ search, lastSessionsView: "projects" }),
      ).toBeNull();
    }
  });
});
