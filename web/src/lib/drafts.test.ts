import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  __resetDraftsForTests,
  clearDraft,
  readDraft,
  stableAnchorKey,
  writeDraft,
} from "./drafts";

// vitest runs in the node env here (no DOM) — stub a minimal in-memory
// localStorage, same pattern as api/prefs.test.ts.
function stubLocalStorage() {
  const store = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => {
      store.set(k, v);
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    get length() {
      return store.size;
    },
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    clear: () => store.clear(),
  });
  return store;
}

beforeEach(() => {
  __resetDraftsForTests();
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe("readDraft/writeDraft/clearDraft — storage round trip", () => {
  it("reads back exactly what was written (the type→reload→restored flow)", () => {
    stubLocalStorage();
    writeDraft("kb1", "art1", "file", "half-typed comment");
    // A fresh `readDraft` call is exactly what a reloaded page does — no
    // in-memory state survives a reload, only what's in storage.
    expect(readDraft("kb1", "art1", "file")).toBe("half-typed comment");
  });

  it("returns '' for a slot that was never written", () => {
    stubLocalStorage();
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });

  it("writes under the documented key grammar", () => {
    const store = stubLocalStorage();
    writeDraft("mykb", "a_deadbeef0001", "compose:section:h1::", "hi");
    expect(store.has("kb-draft/1:mykb:a_deadbeef0001:compose:section:h1::")).toBe(
      true,
    );
  });

  it("keeps distinct slots for the same artifact independent", () => {
    stubLocalStorage();
    writeDraft("kb1", "art1", "file", "file draft");
    writeDraft("kb1", "art1", "reply:c_1", "reply draft");
    expect(readDraft("kb1", "art1", "file")).toBe("file draft");
    expect(readDraft("kb1", "art1", "reply:c_1")).toBe("reply draft");
  });

  it("keeps the same slot independent across artifacts", () => {
    stubLocalStorage();
    writeDraft("kb1", "art1", "file", "draft on art1");
    writeDraft("kb1", "art2", "file", "draft on art2");
    expect(readDraft("kb1", "art1", "file")).toBe("draft on art1");
    expect(readDraft("kb1", "art2", "file")).toBe("draft on art2");
  });

  it("writing an empty string clears the slot (a blank draft is no draft)", () => {
    const store = stubLocalStorage();
    writeDraft("kb1", "art1", "file", "something");
    writeDraft("kb1", "art1", "file", "");
    expect(readDraft("kb1", "art1", "file")).toBe("");
    expect(store.has("kb-draft/1:kb1:art1:file")).toBe(false);
  });

  it("clearDraft removes a slot outright", () => {
    stubLocalStorage();
    writeDraft("kb1", "art1", "file", "something");
    clearDraft("kb1", "art1", "file");
    expect(readDraft("kb1", "art1", "file")).toBe("");
  });

  it("every access is a silent no-op when localStorage throws (private mode / quota)", () => {
    vi.stubGlobal("localStorage", {
      getItem: () => {
        throw new Error("SecurityError");
      },
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
      removeItem: () => {
        throw new Error("SecurityError");
      },
      get length(): number {
        throw new Error("SecurityError");
      },
      key: () => {
        throw new Error("SecurityError");
      },
    });
    expect(() => writeDraft("kb1", "art1", "file", "x")).not.toThrow();
    expect(readDraft("kb1", "art1", "file")).toBe("");
    expect(() => clearDraft("kb1", "art1", "file")).not.toThrow();
  });
});

describe("TTL — 7-day lazy sweep on first read", () => {
  it("an expired draft reads back as '' and is actually removed from storage", () => {
    const store = stubLocalStorage();
    vi.useFakeTimers();
    vi.setSystemTime(0);
    writeDraft("kb1", "art1", "file", "old draft");
    // 8 days later — past the 7-day TTL.
    vi.setSystemTime(8 * 24 * 60 * 60 * 1000);
    expect(readDraft("kb1", "art1", "file")).toBe("");
    expect(store.has("kb-draft/1:kb1:art1:file")).toBe(false);
  });

  it("a fresh draft (< 7 days old) survives the sweep", () => {
    stubLocalStorage();
    vi.useFakeTimers();
    vi.setSystemTime(0);
    writeDraft("kb1", "art1", "file", "recent draft");
    vi.setSystemTime(6 * 24 * 60 * 60 * 1000);
    expect(readDraft("kb1", "art1", "file")).toBe("recent draft");
  });

  it("sweeps every expired kb-draft key on the first read, not just the one requested", () => {
    const store = stubLocalStorage();
    vi.useFakeTimers();
    vi.setSystemTime(0);
    writeDraft("kb1", "art1", "file", "stale");
    writeDraft("kb1", "art2", "reply:c_9", "also stale");
    vi.setSystemTime(8 * 24 * 60 * 60 * 1000);
    // Reading a THIRD, unrelated slot still sweeps the other two.
    readDraft("kb1", "art3", "file");
    expect(store.has("kb-draft/1:kb1:art1:file")).toBe(false);
    expect(store.has("kb-draft/1:kb1:art2:reply:c_9")).toBe(false);
  });

  it("the sweep runs at most once per module life (until reset) — a latch, not per-call", () => {
    const store = stubLocalStorage();
    vi.useFakeTimers();
    vi.setSystemTime(0);
    writeDraft("kb1", "art1", "file", "first");
    readDraft("kb1", "art1", "file"); // trips the latch, sweeps (nothing stale yet)

    // Advance well past the TTL, then write a SECOND draft directly into
    // the past (simulating it having been written before the trip too) —
    // the latch means a further readDraft this session won't re-sweep it.
    vi.setSystemTime(1000);
    store.set(
      "kb-draft/1:kb1:art2:file",
      JSON.stringify({ text: "second", savedAt: 1000 }),
    );
    vi.setSystemTime(1000 + 8 * 24 * 60 * 60 * 1000);
    readDraft("kb1", "art3", "file"); // does NOT re-sweep — latch already tripped
    expect(store.has("kb-draft/1:kb1:art2:file")).toBe(true);
    // readDraft on the expired key ITSELF still returns "" (per-read TTL
    // check is independent of the sweep latch) even though the sweep
    // didn't delete it from storage this session.
    expect(readDraft("kb1", "art2", "file")).toBe("");
  });

  it("ignores unparseable entries under the prefix as expired (drop, don't throw)", () => {
    const store = stubLocalStorage();
    store.set("kb-draft/1:kb1:art1:file", "not json");
    expect(() => readDraft("kb1", "art9", "file")).not.toThrow();
    expect(store.has("kb-draft/1:kb1:art1:file")).toBe(false);
  });
});

describe("stableAnchorKey — deterministic serialization for the compose slot", () => {
  it("is the bare string 'file' for a file anchor", () => {
    expect(stableAnchorKey({ kind: "file" })).toBe("file");
  });

  it("is deterministic for the same chapter anchor", () => {
    const a = { kind: "chapter" as const, path: "ch/1.html" };
    expect(stableAnchorKey(a)).toBe(stableAnchorKey({ ...a }));
  });

  it("distinguishes different section ids", () => {
    const a = stableAnchorKey({ kind: "section", id: "h1", tag: null, snippet: null });
    const b = stableAnchorKey({ kind: "section", id: "h2", tag: null, snippet: null });
    expect(a).not.toBe(b);
  });

  it("distinguishes anchors that differ only by kind", () => {
    const chapter = stableAnchorKey({ kind: "chapter", path: "x" });
    const section = stableAnchorKey({
      kind: "section",
      id: "x",
      tag: null,
      snippet: null,
    });
    expect(chapter).not.toBe(section);
  });

  it("is structurally stable for a selection anchor over css_path/offset/snippet", () => {
    const anchor = {
      kind: "selection" as const,
      css_path: "div > p:nth-child(2)",
      offset: 14,
      snippet: "hello world",
    };
    expect(stableAnchorKey(anchor)).toBe(stableAnchorKey({ ...anchor }));
    expect(stableAnchorKey(anchor)).not.toBe(
      stableAnchorKey({ ...anchor, offset: 15 }),
    );
  });
});
