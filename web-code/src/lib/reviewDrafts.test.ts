import { describe, expect, it } from "vitest";
import type { StorageLike } from "../desk/deskState";
import {
  clearDrafts,
  draftCountByPath,
  draftsInSpan,
  draftsStorageKey,
  draftsToBatch,
  EMPTY_DRAFTS,
  loadDrafts,
  MAX_PUBLISH_OPS,
  newDraftId,
  publishRefusal,
  removeDraft,
  REVIEW_DRAFTS_VERSION,
  saveDrafts,
  upsertDraft,
  wireIntent,
  type DraftsState,
  type ReviewDraft,
} from "./reviewDrafts";

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

function draft(over: Partial<ReviewDraft> = {}): ReviewDraft {
  return {
    id: "d-1",
    path: "src/lib.rs",
    side: "new",
    line: 12,
    intent: "comment",
    body: "this races",
    createdAt: 1_700_000_000_000,
    ...over,
  };
}

function state(...drafts: ReviewDraft[]): DraftsState {
  return { v: REVIEW_DRAFTS_VERSION, drafts };
}

describe("the storage key", () => {
  it("is per repo AND per review", () => {
    expect(draftsStorageKey("kb", 7)).toBe("kbc:review-drafts:kb:7");
    expect(draftsStorageKey("kb", 8)).not.toBe(draftsStorageKey("kb", 7));
    expect(draftsStorageKey("other", 7)).not.toBe(draftsStorageKey("kb", 7));
  });
});

describe("round trip", () => {
  it("saves and restores — the reload survival the tray promises", () => {
    const s = mem();
    saveDrafts("kb", 7, state(draft(), draft({ id: "d-2", line: 40 })), s);
    expect(loadDrafts("kb", 7, s).drafts).toHaveLength(2);
    expect(loadDrafts("kb", 7, s).drafts[1]).toEqual(draft({ id: "d-2", line: 40 }));
  });

  it("an empty tray REMOVES the key rather than leaving an empty blob", () => {
    const s = mem();
    saveDrafts("kb", 7, state(draft()), s);
    saveDrafts("kb", 7, state(), s);
    expect(s.data[draftsStorageKey("kb", 7)]).toBeUndefined();
  });

  it("no storage at all is the empty tray, never a crash", () => {
    expect(loadDrafts("kb", 7, null)).toEqual(EMPTY_DRAFTS);
    expect(() => saveDrafts("kb", 7, state(draft()), null)).not.toThrow();
  });
});

describe("loadDrafts is TOTAL", () => {
  const key = draftsStorageKey("kb", 7);
  const bad: Array<[string, string]> = [
    ["unparseable JSON", "{nope"],
    ["a JSON scalar", '"hello"'],
    ["null", "null"],
    ["a wrong version", JSON.stringify({ v: 99, drafts: [draft()] })],
    ["a non-array drafts field", JSON.stringify({ v: 1, drafts: { a: 1 } })],
  ];
  for (const [name, blob] of bad) {
    it(`${name} degrades to the empty tray`, () => {
      expect(loadDrafts("kb", 7, mem({ [key]: blob }))).toEqual(EMPTY_DRAFTS);
    });
  }

  it("keeps the VALID rows of a partially-corrupt blob", () => {
    // Losing seven good drafts because an eighth is malformed would be the
    // worse failure — this module's own header says so.
    const blob = JSON.stringify({
      v: REVIEW_DRAFTS_VERSION,
      drafts: [draft(), { id: "x" }, { ...draft({ id: "d-3" }), side: "sideways" as unknown as "new" }, draft({ id: "d-4" })],
    });
    expect(loadDrafts("kb", 7, mem({ [key]: blob })).drafts.map((d) => d.id)).toEqual([
      "d-1",
      "d-4",
    ]);
  });

  it("a storage that throws is the empty tray", () => {
    const throwing: StorageLike = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {},
      removeItem: () => {},
    };
    expect(loadDrafts("kb", 7, throwing)).toEqual(EMPTY_DRAFTS);
  });
});

describe("mutations", () => {
  it("upsert APPENDS a new draft and EDITS in place", () => {
    const one = upsertDraft(state(), draft());
    expect(one.drafts).toHaveLength(1);
    const two = upsertDraft(one, draft({ id: "d-2" }));
    const edited = upsertDraft(two, draft({ body: "edited" }));
    expect(edited.drafts.map((d) => d.id)).toEqual(["d-1", "d-2"]);
    expect(edited.drafts[0].body).toBe("edited");
  });

  it("remove and clear", () => {
    const two = upsertDraft(upsertDraft(state(), draft()), draft({ id: "d-2" }));
    expect(removeDraft(two, "d-1").drafts.map((d) => d.id)).toEqual(["d-2"]);
    expect(clearDrafts(two).drafts).toEqual([]);
  });

  it("newDraftId is unique-ish and prefixed", () => {
    const ids = new Set(Array.from({ length: 50 }, () => newDraftId()));
    expect(ids.size).toBe(50);
    for (const id of ids) expect(id.startsWith("d-")).toBe(true);
  });
});

describe("per-file and per-hunk counts", () => {
  it("counts by path", () => {
    const s = state(draft(), draft({ id: "d-2" }), draft({ id: "d-3", path: "b.rs" }));
    expect([...draftCountByPath(s)]).toEqual([
      ["src/lib.rs", 2],
      ["b.rs", 1],
    ]);
  });

  it("finds drafts inside a hunk's span, on that side only", () => {
    const s = state(
      draft({ id: "d-1", line: 12, side: "new" }),
      draft({ id: "d-2", line: 12, side: "old" }),
      draft({ id: "d-3", line: 99, side: "new" }),
    );
    expect(draftsInSpan(s, "src/lib.rs", "new", 10, 20).map((d) => d.id)).toEqual(["d-1"]);
    expect(draftsInSpan(s, "other.rs", "new", 10, 20)).toEqual([]);
  });
});

describe("the publish batch shape", () => {
  it("maps the draft vocabulary onto the WIRE's annotation intents", () => {
    expect(wireIntent("comment")).toBe("note");
    expect(wireIntent("question")).toBe("question");
  });

  it("is one add_comment op per draft, in tray order", () => {
    const req = draftsToBatch("kb", 7, 3, [
      draft(),
      draft({ id: "d-2", path: "b.rs", side: "old", line: 4, intent: "question", body: "why?" }),
    ]);
    expect(req).toEqual({
      repo: "kb",
      ops: [
        {
          op: "add_comment",
          path: "src/lib.rs",
          line: 12,
          body: "this races",
          anchor_kind: "line",
          intent: "note",
          review_id: 7,
          side: "new",
          ps: 3,
        },
        {
          op: "add_comment",
          path: "b.rs",
          line: 4,
          body: "why?",
          anchor_kind: "line",
          intent: "question",
          review_id: 7,
          side: "old",
          ps: 3,
        },
      ],
    });
  });

  it("omits `ps` for latest — an absent ps is the server's own default", () => {
    const req = draftsToBatch("kb", 7, null, [draft()]);
    expect(req.ops[0].ps).toBeUndefined();
  });

  it("carries a range anchor and a suggestion when the draft has them", () => {
    const req = draftsToBatch("kb", 7, null, [
      draft({ lineEnd: 18, suggestion: "let x = 2;" }),
    ]);
    expect(req.ops[0]).toMatchObject({
      anchor_kind: "range",
      line: 12,
      line_end: 18,
      suggestion: { replacement: "let x = 2;" },
    });
  });

  it("a lineEnd that is not past line stays a plain line anchor", () => {
    const req = draftsToBatch("kb", 7, null, [draft({ lineEnd: 12 })]);
    expect(req.ops[0].anchor_kind).toBe("line");
    expect(req.ops[0].line_end).toBeUndefined();
  });
});

describe("publishRefusal", () => {
  it("refuses an empty tray", () => {
    expect(publishRefusal([])).toBe("no drafts to publish");
  });

  it("refuses past the server's own batch cap, naming both numbers", () => {
    const many = Array.from({ length: MAX_PUBLISH_OPS + 1 }, (_, i) => draft({ id: `d-${i}` }));
    const refusal = publishRefusal(many);
    expect(refusal).toContain(String(MAX_PUBLISH_OPS + 1));
    expect(refusal).toContain(String(MAX_PUBLISH_OPS));
  });

  it("refuses an empty body rather than posting one", () => {
    expect(publishRefusal([draft({ body: "   " })])).toBe("1 draft(s) have an empty body");
  });

  it("allows a well-formed tray", () => {
    expect(publishRefusal([draft()])).toBeNull();
  });
});
