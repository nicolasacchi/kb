import { describe, it, expect } from "vitest";
import { mergeConfig } from "./config";

// mergeConfig is draft-authoritative for the key set at EVERY level:
// a key present in `base` but absent from `draft` is dropped (so an
// editor deletion of a Record entry persists), object values merge
// recursively, and arrays / scalars / explicit nulls from `draft`
// replace `base` wholesale. These tests pin the R2 regression — a
// removed per-kb `templates` entry must survive the deep-merge rather
// than being re-merged back in from `base`.

describe("mergeConfig", () => {
  it("drops a top-level key present in base but absent from draft", () => {
    expect(mergeConfig<Record<string, unknown>>({ a: 1, b: 2 }, { a: 1 })).toEqual({ a: 1 });
  });

  it("persists a nested Record deletion (the R2 regression)", () => {
    const base = { kb: { x: { templates: { t1: "a", t2: "b" } } } };
    const draft = { kb: { x: { templates: { t1: "a" } } } };
    expect(mergeConfig(base, draft)).toEqual({
      kb: { x: { templates: { t1: "a" } } },
    });
  });

  it("replaces arrays wholesale rather than merging them", () => {
    expect(mergeConfig<number[]>([1, 2, 3], [1])).toEqual([1]);
  });

  it("replaces a scalar with the draft value", () => {
    expect(mergeConfig<Record<string, unknown>>({ n: 5 }, { n: 9 })).toEqual({ n: 9 });
  });

  it("lets an explicit null in draft overwrite an object in base", () => {
    expect(mergeConfig<{ o: unknown }>({ o: { a: 1 } }, { o: null })).toEqual({ o: null });
  });

  it("uses the draft object when base holds null (not an object)", () => {
    expect(mergeConfig<{ o: unknown }>({ o: null }, { o: { a: 1 } })).toEqual({ o: { a: 1 } });
  });

  it("keeps a draft-only key that base does not have", () => {
    expect(mergeConfig<Record<string, unknown>>({ a: 1 }, { a: 1, b: 2 })).toEqual({ a: 1, b: 2 });
  });

  it("returns the draft object when base is a scalar at that key", () => {
    expect(mergeConfig<{ x: unknown }>({ x: 5 }, { x: { a: 1 } })).toEqual({ x: { a: 1 } });
  });

  it("returns the draft scalar when base is an object at that key", () => {
    expect(mergeConfig<{ x: unknown }>({ x: { a: 1 } }, { x: 5 })).toEqual({ x: 5 });
  });

  it("replaces wholesale when either side is an array", () => {
    expect(mergeConfig<number[]>([1, 2], [3])).toEqual([3]);
  });

  it("handles two empty objects", () => {
    expect(mergeConfig<Record<string, unknown>>({}, {})).toEqual({});
  });

  it("does not mutate its inputs", () => {
    const base = { kb: { x: { templates: { t1: "a", t2: "b" } } } };
    const draft = { kb: { x: { templates: { t1: "a" } } } };
    const baseSnap = structuredClone(base);
    const draftSnap = structuredClone(draft);
    mergeConfig(base, draft);
    expect(base).toEqual(baseSnap);
    expect(draft).toEqual(draftSnap);
  });

  it("preserves an unknown future field the draft carried from base", () => {
    // The draft is a structuredClone of the server GET, so it always
    // carries server-only fields this client type doesn't model.
    const base = { server: { addr: "x", futureField: 42 } };
    const draft = { server: { addr: "y", futureField: 42 } };
    expect(mergeConfig(base, draft)).toEqual({
      server: { addr: "y", futureField: 42 },
    });
  });

  it("round-trips a realistic per-kb edit: templates entry removed, siblings kept", () => {
    // Mirrors the production shape (DaemonConfig.tsx clones the server GET,
    // the templates sub-editor re-emits the kb section). Deleting one
    // `templates` entry must persist while every sibling field survives.
    const base = {
      kb: {
        notes: {
          path: "/srv/notes",
          skip_patterns: ["*.tmp"],
          templates: { default: "/t/default.html", review: "/t/review.html" },
          versions: "auto",
        },
      },
    };
    const draft = {
      kb: {
        notes: {
          path: "/srv/notes",
          skip_patterns: ["*.tmp"],
          templates: { default: "/t/default.html" },
          versions: "auto",
        },
      },
    };
    expect(mergeConfig(base, draft)).toEqual({
      kb: {
        notes: {
          path: "/srv/notes",
          skip_patterns: ["*.tmp"],
          templates: { default: "/t/default.html" },
          versions: "auto",
        },
      },
    });
  });
});
