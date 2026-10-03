import { describe, expect, it } from "vitest";
import type { StorageLike } from "../desk/deskState";
import {
  __resetCurrentReviewForTests,
  clearCurrentReview,
  clearCurrentReviewIfSet,
  currentReviewStorageKey,
  CURRENT_REVIEW_VERSION,
  getCurrentReview,
  peekCurrentReview,
  setCurrentReview,
} from "./currentReview";

// Same fake-storage idiom `reviewDrafts.test.ts` uses.
function mem(seed: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
  const data = { ...seed };
  return {
    data,
    getItem: (k) => (k in data ? data[k] : null),
    setItem: (k, v) => {
      data[k] = v;
    },
    removeItem: (k) => {
      delete data[k];
    },
  };
}

describe("currentReviewStorageKey", () => {
  it("is per repo", () => {
    expect(currentReviewStorageKey("kb")).toBe("kbc:current-review:kb");
    expect(currentReviewStorageKey("other")).not.toBe(currentReviewStorageKey("kb"));
  });
});

describe("set / get / clear round trip", () => {
  it("returns null when nothing is set", () => {
    const s = mem();
    expect(getCurrentReview("kb", s)).toBeNull();
  });

  it("round-trips id + title", () => {
    const s = mem();
    setCurrentReview("kb", { id: "7", title: "e2e review" }, s);
    expect(getCurrentReview("kb", s)).toEqual({ id: "7", title: "e2e review", setAt: expect.any(Number) });
  });

  it("round-trips an id-only marker (cold ?review= load, title not yet known)", () => {
    const s = mem();
    setCurrentReview("kb", { id: "7" }, s);
    const got = getCurrentReview("kb", s);
    expect(got?.id).toBe("7");
    expect(got?.title).toBeUndefined();
  });

  it("accepts an explicit setAt for deterministic assertions", () => {
    const s = mem();
    setCurrentReview("kb", { id: "1", setAt: 1_700_000_000_000 }, s);
    expect(getCurrentReview("kb", s)?.setAt).toBe(1_700_000_000_000);
  });

  it("clear removes the marker entirely", () => {
    const s = mem();
    setCurrentReview("kb", { id: "7" }, s);
    clearCurrentReview("kb", s);
    expect(getCurrentReview("kb", s)).toBeNull();
    expect(s.data[currentReviewStorageKey("kb")]).toBeUndefined();
  });

  it("a later set replaces the earlier one for the same repo", () => {
    const s = mem();
    setCurrentReview("kb", { id: "1" }, s);
    setCurrentReview("kb", { id: "2" }, s);
    expect(getCurrentReview("kb", s)?.id).toBe("2");
  });

  it("setCurrentReview with an empty id is a no-op", () => {
    const s = mem();
    setCurrentReview("kb", { id: "" }, s);
    expect(getCurrentReview("kb", s)).toBeNull();
  });
});

describe("other-repo isolation", () => {
  it("two repos sharing ONE storage instance never see each other's marker", () => {
    const s = mem();
    setCurrentReview("kb", { id: "1", title: "kb review" }, s);
    setCurrentReview("other", { id: "2", title: "other review" }, s);
    expect(getCurrentReview("kb", s)?.id).toBe("1");
    expect(getCurrentReview("other", s)?.id).toBe("2");
    clearCurrentReview("kb", s);
    expect(getCurrentReview("kb", s)).toBeNull();
    expect(getCurrentReview("other", s)?.id).toBe("2");
  });
});

describe("corrupt / unreadable blobs degrade to null, never throw", () => {
  it("missing key", () => {
    expect(getCurrentReview("kb", mem())).toBeNull();
  });

  it("unparseable JSON", () => {
    expect(getCurrentReview("kb", mem({ [currentReviewStorageKey("kb")]: "{not json" }))).toBeNull();
  });

  it("a JSON value that isn't an object", () => {
    expect(getCurrentReview("kb", mem({ [currentReviewStorageKey("kb")]: "42" }))).toBeNull();
  });

  it("a future/unknown version is refused rather than misread", () => {
    const blob = JSON.stringify({ v: CURRENT_REVIEW_VERSION + 1, value: { id: "1", setAt: 1 } });
    expect(getCurrentReview("kb", mem({ [currentReviewStorageKey("kb")]: blob }))).toBeNull();
  });

  it("a value missing a required field", () => {
    const blob = JSON.stringify({ v: CURRENT_REVIEW_VERSION, value: { title: "no id" } });
    expect(getCurrentReview("kb", mem({ [currentReviewStorageKey("kb")]: blob }))).toBeNull();
  });

  it("a value with an empty id", () => {
    const blob = JSON.stringify({ v: CURRENT_REVIEW_VERSION, value: { id: "", setAt: 1 } });
    expect(getCurrentReview("kb", mem({ [currentReviewStorageKey("kb")]: blob }))).toBeNull();
  });

  it("a storage that throws on access never crashes the read", () => {
    const throwing: StorageLike = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {
        throw new Error("blocked");
      },
    };
    expect(getCurrentReview("kb", throwing)).toBeNull();
    expect(() => setCurrentReview("kb", { id: "1" }, throwing)).not.toThrow();
    expect(() => clearCurrentReview("kb", throwing)).not.toThrow();
  });

  it("a null storage (no sessionStorage available) never crashes", () => {
    expect(getCurrentReview("kb", null)).toBeNull();
    expect(() => setCurrentReview("kb", { id: "1" }, null)).not.toThrow();
    expect(() => clearCurrentReview("kb", null)).not.toThrow();
  });
});

// A9.f5 -- N surfaces mount the validated hook and each runs the "review is
// gone" effect; exactly one may clear (one URL strip, one toast).
describe("clearCurrentReviewIfSet / peekCurrentReview dedupe", () => {
  it("lets exactly one of several same-tick callers clear the marker", () => {
    __resetCurrentReviewForTests();
    const s = mem();
    setCurrentReview("kb", { id: "7" }, s);
    expect(peekCurrentReview("kb")?.id).toBe("7");
    // reader, search, omnibox, top bar, annotations rail
    const results = [1, 2, 3, 4, 5].map(() => clearCurrentReviewIfSet("kb", s));
    expect(results).toEqual([true, false, false, false, false]);
    expect(peekCurrentReview("kb")).toBeNull();
    expect(getCurrentReview("kb", s)).toBeNull();
  });

  it("reads the in-memory cache, not storage, so a failed storage write still dedupes", () => {
    __resetCurrentReviewForTests();
    const full: StorageLike = {
      getItem: () => null,
      setItem: () => {
        throw new Error("QuotaExceededError");
      },
      removeItem: () => {},
    };
    setCurrentReview("kb", { id: "9" }, full);
    expect(getCurrentReview("kb", full)).toBeNull(); // storage never got it
    expect(peekCurrentReview("kb")?.id).toBe("9"); // the cache did
    expect(clearCurrentReviewIfSet("kb", full)).toBe(true);
    expect(clearCurrentReviewIfSet("kb", full)).toBe(false);
  });

  it("does nothing for a repo with no marker", () => {
    __resetCurrentReviewForTests();
    expect(clearCurrentReviewIfSet("nothing-here", mem())).toBe(false);
  });
});
