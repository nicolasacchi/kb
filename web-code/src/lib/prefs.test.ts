import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  clampReaderFontSize,
  coldSeedRepo,
  loadAttentionOverlay,
  loadDiffSyntaxHighlight,
  loadLastRepo,
  loadPrefs,
  loadReaderFontSize,
  loadWrap,
  READER_FONT_SIZE_DEFAULT,
  READER_FONT_SIZE_MAX,
  READER_FONT_SIZE_MIN,
  saveAttentionOverlay,
  saveDiffSyntaxHighlight,
  saveLastRepo,
  saveReaderFontSize,
  saveWrap,
} from "./prefs";

// vitest runs in the node env here (no DOM), so stub a minimal in-memory
// localStorage for the persistence round-trip — same approach kb's own
// `web/src/api/prefs.test.ts` uses.
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

describe("coldSeedRepo (cold-entry gating)", () => {
  const REPOS = ["alpha", "beta", "gamma"]; // alpha is the repos[0] default

  it("seeds the remembered repo on a bare cold entry", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/", lastRepo: "beta" }),
    ).toBe("beta");
  });

  it("does nothing while the repo list is still loading", () => {
    expect(
      coldSeedRepo({ repos: [], explicitRepo: null, pathname: "/", lastRepo: "beta" }),
    ).toBeNull();
  });

  it("yields to an explicit selection", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: "gamma", pathname: "/", lastRepo: "beta" }),
    ).toBeNull();
  });

  it("only seeds on the bare '/' path", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/search", lastRepo: "beta" }),
    ).toBeNull();
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/r/beta/x.rs", lastRepo: "beta" }),
    ).toBeNull();
  });

  it("no-ops when there is nothing remembered", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/", lastRepo: null }),
    ).toBeNull();
  });

  it("no-ops when the remembered repo is already the repos[0] default", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/", lastRepo: "alpha" }),
    ).toBeNull();
  });

  it("ghost-repo guard: ignores a remembered repo no longer configured", () => {
    expect(
      coldSeedRepo({ repos: REPOS, explicitRepo: null, pathname: "/", lastRepo: "deleted" }),
    ).toBeNull();
  });
});

describe("lastRepo persistence", () => {
  beforeEach(() => stubLocalStorage());

  it("round-trips through localStorage", () => {
    expect(loadLastRepo()).toBeNull();
    saveLastRepo("beta");
    expect(loadLastRepo()).toBe("beta");
  });

  it("is a no-op write when unchanged (doesn't thrash localStorage)", () => {
    saveLastRepo("beta");
    const before = JSON.stringify(loadPrefs());
    saveLastRepo("beta");
    expect(JSON.stringify(loadPrefs())).toBe(before);
  });

  it("degrades to defaults on corrupt JSON rather than throwing", () => {
    localStorage.setItem("kbc:prefs", "{not json");
    expect(loadLastRepo()).toBeNull();
  });
});

describe("diffSyntaxHighlight pref (V4.D2)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults ON", () => {
    expect(loadDiffSyntaxHighlight()).toBe(true);
  });

  it("round-trips false", () => {
    saveDiffSyntaxHighlight(false);
    expect(loadDiffSyntaxHighlight()).toBe(false);
    saveDiffSyntaxHighlight(true);
    expect(loadDiffSyntaxHighlight()).toBe(true);
  });
});

describe("attentionOverlay pref (V3.2-B3)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults OFF", () => {
    expect(loadAttentionOverlay()).toBe(false);
  });

  it("round-trips true", () => {
    saveAttentionOverlay(true);
    expect(loadAttentionOverlay()).toBe(true);
    saveAttentionOverlay(false);
    expect(loadAttentionOverlay()).toBe(false);
  });
});

describe("wrap pref (SH.C3)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults OFF", () => {
    expect(loadWrap()).toBe(false);
  });

  it("round-trips true", () => {
    saveWrap(true);
    expect(loadWrap()).toBe(true);
    saveWrap(false);
    expect(loadWrap()).toBe(false);
  });

  it("is a no-op write when unchanged (doesn't thrash localStorage)", () => {
    saveWrap(true);
    const before = JSON.stringify(loadPrefs());
    saveWrap(true);
    expect(JSON.stringify(loadPrefs())).toBe(before);
  });
});

describe("readerFontSize pref (SH.C3)", () => {
  beforeEach(() => stubLocalStorage());

  it("defaults to READER_FONT_SIZE_DEFAULT", () => {
    expect(loadReaderFontSize()).toBe(READER_FONT_SIZE_DEFAULT);
  });

  it("round-trips an in-range value", () => {
    saveReaderFontSize(16);
    expect(loadReaderFontSize()).toBe(16);
  });

  it("clamps below the minimum on save", () => {
    expect(saveReaderFontSize(0)).toBe(READER_FONT_SIZE_MIN);
    expect(loadReaderFontSize()).toBe(READER_FONT_SIZE_MIN);
  });

  it("clamps above the maximum on save", () => {
    expect(saveReaderFontSize(99)).toBe(READER_FONT_SIZE_MAX);
    expect(loadReaderFontSize()).toBe(READER_FONT_SIZE_MAX);
  });

  it("clampReaderFontSize rounds fractional pixels", () => {
    expect(clampReaderFontSize(13.6)).toBe(14);
  });

  it("clampReaderFontSize degrades non-finite input to the default", () => {
    expect(clampReaderFontSize(Number.NaN)).toBe(READER_FONT_SIZE_DEFAULT);
    expect(clampReaderFontSize(Number.POSITIVE_INFINITY)).toBe(READER_FONT_SIZE_DEFAULT);
  });

  it("degrades a corrupt stored value to the default", () => {
    localStorage.setItem("kbc:prefs", JSON.stringify({ readerFontSize: "not-a-number" }));
    expect(loadReaderFontSize()).toBe(READER_FONT_SIZE_DEFAULT);
  });
});
