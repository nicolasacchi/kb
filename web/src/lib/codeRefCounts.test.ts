// CT-E3 tier (a) — the gallery ref-chip join: the hard-capped coderef/1
// feed walk (`collectCodeRefCounts`) and its all-or-nothing honesty gate
// (`galleryRefCount`). Pure over an injected page fetcher — no network.
import { describe, expect, it } from "vitest";
import type { CodeRefsFeedResponse } from "../api/generated/CodeRefsFeedResponse";
import type { CodeRefsDocOut } from "../api/generated/CodeRefsDocOut";
import {
  collectCodeRefCounts,
  galleryRefCount,
  type CodeRefCounts,
} from "./codeRefCounts";

function headerDoc(
  id: string,
  refCount: number,
  neverScanned = false,
): CodeRefsDocOut {
  return {
    doc_id: id,
    doc_path: `${id}.html`,
    title: id,
    doc_hash: neverScanned ? null : "hash",
    extracted_at: neverScanned ? null : 1_754_563_200,
    never_scanned: neverScanned,
    code_rev: null,
    ref_count: refCount,
    ungrouped_count: 0,
    truncated: false,
    groups: [],
    refs: [],
  };
}

function page(
  docs: CodeRefsDocOut[],
  nextCursor?: string,
): CodeRefsFeedResponse {
  const res: CodeRefsFeedResponse = { schema: "coderef-feed/1", kb: "demo", docs };
  // Mirror the wire: `next_cursor` is OMITTED (not null) on the last page.
  if (nextCursor !== undefined) res.next_cursor = nextCursor;
  return res;
}

/// Injectable pager over a fixed page list, recording the cursors it saw.
function pagerOver(pages: CodeRefsFeedResponse[]) {
  const cursors: (string | undefined)[] = [];
  let i = 0;
  return {
    cursors,
    fetchPage: (cursor: string | undefined) => {
      cursors.push(cursor);
      return Promise.resolve(pages[i++]);
    },
  };
}

describe("collectCodeRefCounts", () => {
  it("a single terminal page (no next_cursor) is complete after one request", async () => {
    const { cursors, fetchPage } = pagerOver([
      page([headerDoc("a".repeat(12), 3), headerDoc("b".repeat(12), 1)]),
    ]);
    const out = await collectCodeRefCounts(fetchPage);
    expect(out.complete).toBe(true);
    expect(out.counts).toEqual({ ["a".repeat(12)]: 3, ["b".repeat(12)]: 1 });
    expect(cursors).toEqual([undefined]);
  });

  it("walks the cursor chain, passing each next_cursor back verbatim", async () => {
    const { cursors, fetchPage } = pagerOver([
      page([headerDoc("aaaaaaaaaaaa", 2)], "100:aaaaaaaaaaaa"),
      page([headerDoc("bbbbbbbbbbbb", 5)], "200:bbbbbbbbbbbb"),
      page([headerDoc("cccccccccccc", 1)]),
    ]);
    const out = await collectCodeRefCounts(fetchPage);
    expect(out.complete).toBe(true);
    expect(Object.keys(out.counts)).toHaveLength(3);
    expect(cursors).toEqual([undefined, "100:aaaaaaaaaaaa", "200:bbbbbbbbbbbb"]);
  });

  it("zero-ref and never-scanned headers never enter the map (no chip = no claim)", async () => {
    const { fetchPage } = pagerOver([
      page([
        headerDoc("aaaaaaaaaaaa", 0), // scanned, genuinely zero refs
        headerDoc("bbbbbbbbbbbb", 0, true), // never scanned at all
        headerDoc("cccccccccccc", 4),
      ]),
    ]);
    const out = await collectCodeRefCounts(fetchPage);
    expect(out.counts).toEqual({ cccccccccccc: 4 });
  });

  it("stops at the page cap with next_cursor pending and reports itself INCOMPLETE", async () => {
    const { cursors, fetchPage } = pagerOver([
      page([headerDoc("aaaaaaaaaaaa", 1)], "1:aaaaaaaaaaaa"),
      page([headerDoc("bbbbbbbbbbbb", 2)], "2:bbbbbbbbbbbb"),
      // Never reached — the cap stops the walk first.
      page([headerDoc("cccccccccccc", 3)]),
    ]);
    const out = await collectCodeRefCounts(fetchPage, 2);
    expect(out.complete).toBe(false);
    expect(cursors).toHaveLength(2);
    // The prefix it DID see is still in the map — but galleryRefCount's
    // gate (below) makes sure no card ever renders from it.
    expect(out.counts).toEqual({ aaaaaaaaaaaa: 1, bbbbbbbbbbbb: 2 });
  });

  it("an empty corpus (no headers at all) completes with an empty map", async () => {
    const { fetchPage } = pagerOver([page([])]);
    const out = await collectCodeRefCounts(fetchPage);
    expect(out).toEqual({ counts: {}, complete: true });
  });
});

describe("galleryRefCount (the all-or-nothing chip gate)", () => {
  const complete: CodeRefCounts = {
    counts: { aaaaaaaaaaaa: 7 },
    complete: true,
  };
  const incomplete: CodeRefCounts = {
    counts: { aaaaaaaaaaaa: 7 },
    complete: false,
  };

  it("returns the count for a completed walk", () => {
    expect(galleryRefCount(complete, "aaaaaaaaaaaa")).toBe(7);
  });

  it("returns 0 for a doc absent from a completed walk", () => {
    expect(galleryRefCount(complete, "zzzzzzzzzzzz")).toBe(0);
  });

  it("returns 0 for EVERY doc when the walk was incomplete — even ones it saw", () => {
    // A partial map can't distinguish "cites nothing" from "fell past the
    // cap", so an over-cap corpus renders no chips anywhere rather than a
    // partial set that reads as a claim.
    expect(galleryRefCount(incomplete, "aaaaaaaaaaaa")).toBe(0);
  });

  it("returns 0 while the query has no data yet", () => {
    expect(galleryRefCount(undefined, "aaaaaaaaaaaa")).toBe(0);
  });
});
