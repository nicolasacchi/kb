import { describe, expect, it } from "vitest";
import type { DocSummary } from "../api/client";
import type { InboxItem } from "../api/inbox";
import {
  aggregateOpenCounts,
  censusLine,
  composePage,
  isoWeekInfo,
  mastheadVolume,
  recencyDecay,
  scoreDoc,
} from "./broadsheet";

function doc(partial: Partial<DocSummary> & { id: string }): DocSummary {
  return {
    title: partial.id,
    path: `${partial.id}.html`,
    folder: "",
    source_relative: `${partial.id}.html`,
    ...partial,
  };
}

function inboxItem(partial: Partial<InboxItem> & { artifact_id: string }): InboxItem {
  return {
    kb: "kb",
    title: partial.artifact_id,
    comment_id: `${partial.artifact_id}-c`,
    excerpt: "",
    author: "you",
    reply_count: 0,
    anchor: null,
    stale: false,
    created_at: 0,
    updated_at: 0,
    ...partial,
  } as InboxItem;
}

const DAY = 86_400;

describe("aggregateOpenCounts", () => {
  it("counts open-comment rows per artifact_id", () => {
    const items = [
      inboxItem({ artifact_id: "a" }),
      inboxItem({ artifact_id: "a" }),
      inboxItem({ artifact_id: "b" }),
    ];
    const m = aggregateOpenCounts(items);
    expect(m.get("a")).toBe(2);
    expect(m.get("b")).toBe(1);
    expect(m.get("missing")).toBeUndefined();
  });

  it("returns an empty map for no items", () => {
    expect(aggregateOpenCounts([]).size).toBe(0);
  });
});

describe("recencyDecay", () => {
  it("returns 0 for a missing mtime", () => {
    expect(recencyDecay(null, Date.now())).toBe(0);
    expect(recencyDecay(undefined, Date.now())).toBe(0);
  });

  it("is 1 at age zero and 0.5 at exactly one half-life", () => {
    const nowMs = 1_700_000_000_000;
    const mtimeUnix = nowMs / 1000;
    expect(recencyDecay(mtimeUnix, nowMs)).toBeCloseTo(1);
    const halfLifeAgo = mtimeUnix - 30 * DAY;
    expect(recencyDecay(halfLifeAgo, nowMs)).toBeCloseTo(0.5, 6);
  });

  it("clamps a future mtime (clock skew) to age zero, never above 1", () => {
    const nowMs = 1_700_000_000_000;
    const future = nowMs / 1000 + DAY;
    expect(recencyDecay(future, nowMs)).toBeCloseTo(1);
  });
});

describe("scoreDoc", () => {
  it("decomposes into word x recency x comment factors", () => {
    const nowMs = 1_700_000_000_000;
    const d = doc({ id: "a", word_count: 999, mtime_unix: nowMs / 1000 });
    const e = scoreDoc(d, 2, nowMs);
    expect(e.wordFactor).toBeCloseTo(Math.log(1000));
    expect(e.recencyFactor).toBeCloseTo(1);
    expect(e.commentFactor).toBeCloseTo(1.5); // 1 + 2/4
    expect(e.score).toBeCloseTo(e.wordFactor * e.recencyFactor * e.commentFactor);
  });

  it("treats missing word_count as zero (score zero, not excluded)", () => {
    const nowMs = 1_700_000_000_000;
    const d = doc({ id: "a", mtime_unix: nowMs / 1000 });
    const e = scoreDoc(d, 0, nowMs);
    expect(e.wordFactor).toBe(0);
    expect(e.score).toBe(0);
  });

  it("caps the comment bonus at 4 open comments", () => {
    const nowMs = 1_700_000_000_000;
    const d = doc({ id: "a", word_count: 100, mtime_unix: nowMs / 1000 });
    expect(scoreDoc(d, 4, nowMs).commentFactor).toBeCloseTo(2);
    expect(scoreDoc(d, 40, nowMs).commentFactor).toBeCloseTo(2);
  });
});

describe("composePage", () => {
  const nowMs = 1_700_000_000_000;
  const nowSecs = nowMs / 1000;

  it("returns nulls/empties for an empty row set", () => {
    const page = composePage([], new Map(), nowMs);
    expect(page.lead).toBeNull();
    expect(page.features).toEqual([]);
    expect(page.briefs).toEqual([]);
    expect(page.coverage).toEqual([]);
  });

  it("picks the top-scoring doc as lead, next 4 as features, next 8 as briefs", () => {
    // 20 rows, descending word_count so score strictly decreases with index
    // (same mtime/comments for every row isolates word_count as the driver).
    const rows = Array.from({ length: 20 }, (_, i) =>
      doc({
        id: `d${String(i).padStart(2, "0")}`,
        word_count: 2000 - i * 50,
        mtime_unix: nowSecs,
      }),
    );
    const page = composePage(rows, new Map(), nowMs);
    expect(page.lead?.doc.id).toBe("d00");
    expect(page.features.map((d) => d.id)).toEqual(["d01", "d02", "d03", "d04"]);
    expect(page.briefs.map((d) => d.id)).toEqual([
      "d05", "d06", "d07", "d08", "d09", "d10", "d11", "d12",
    ]);
  });

  it("breaks a full score tie on id (deterministic total order)", () => {
    const rows = [
      doc({ id: "zeta", word_count: 100, mtime_unix: nowSecs }),
      doc({ id: "alpha", word_count: 100, mtime_unix: nowSecs }),
    ];
    const page = composePage(rows, new Map(), nowMs);
    expect(page.lead?.doc.id).toBe("alpha");
    expect(page.features.map((d) => d.id)).toEqual(["zeta"]);
  });

  it("factors comment mass into the ranking", () => {
    const rows = [
      doc({ id: "quiet", word_count: 500, mtime_unix: nowSecs }),
      doc({ id: "discussed", word_count: 500, mtime_unix: nowSecs }),
    ];
    const commentMass = new Map([["discussed", 4]]);
    const page = composePage(rows, commentMass, nowMs);
    expect(page.lead?.doc.id).toBe("discussed");
  });

  it("retains the score decomposition on the lead result", () => {
    const rows = [doc({ id: "a", word_count: 500, mtime_unix: nowSecs })];
    const page = composePage(rows, new Map(), nowMs);
    expect(page.lead?.explain.score).toBeGreaterThan(0);
    expect(page.lead?.explain).toHaveProperty("wordFactor");
    expect(page.lead?.explain).toHaveProperty("recencyFactor");
    expect(page.lead?.explain).toHaveProperty("commentFactor");
  });

  it("continuing coverage: mtime-after-last-open rows, newest first, capped at 6", () => {
    const rows = [
      // never opened — excluded (no last_opened_unix)
      doc({ id: "never", mtime_unix: nowSecs }),
      // opened, but not updated since — excluded
      doc({ id: "stale-read", mtime_unix: nowSecs - 1000, last_opened_unix: nowSecs }),
      // updated after last open — included, older
      doc({ id: "older", mtime_unix: nowSecs - 100, last_opened_unix: nowSecs - 200 }),
      // updated after last open — included, newer
      doc({ id: "newer", mtime_unix: nowSecs - 10, last_opened_unix: nowSecs - 200 }),
    ];
    const page = composePage(rows, new Map(), nowMs);
    expect(page.coverage.map((d) => d.id)).toEqual(["newer", "older"]);
  });

  it("caps continuing coverage at 6 with a deterministic id tiebreak", () => {
    const rows = Array.from({ length: 9 }, (_, i) =>
      doc({
        id: `u${i}`,
        mtime_unix: nowSecs, // identical mtime -> id tiebreak decides order
        last_opened_unix: nowSecs - 200,
      }),
    );
    const page = composePage(rows, new Map(), nowMs);
    expect(page.coverage).toHaveLength(6);
    expect(page.coverage.map((d) => d.id)).toEqual([
      "u0", "u1", "u2", "u3", "u4", "u5",
    ]);
  });
});

describe("isoWeekInfo", () => {
  // Cross-checked against the standard ISO-8601 week algorithm
  // (Thursday-anchored) for every classic year-boundary case.
  const cases: [string, { isoYear: number; week: number }][] = [
    ["2015-12-28T00:00:00", { isoYear: 2015, week: 53 }],
    ["2015-12-31T00:00:00", { isoYear: 2015, week: 53 }],
    ["2016-01-01T00:00:00", { isoYear: 2015, week: 53 }],
    ["2016-01-04T00:00:00", { isoYear: 2016, week: 1 }],
    ["2020-12-28T00:00:00", { isoYear: 2020, week: 53 }],
    ["2020-12-31T00:00:00", { isoYear: 2020, week: 53 }],
    ["2021-01-01T00:00:00", { isoYear: 2020, week: 53 }],
    ["2021-01-04T00:00:00", { isoYear: 2021, week: 1 }],
    ["2018-01-01T00:00:00", { isoYear: 2018, week: 1 }],
    ["2018-12-31T00:00:00", { isoYear: 2019, week: 1 }],
    ["2026-01-01T00:00:00", { isoYear: 2026, week: 1 }],
    ["2026-12-28T00:00:00", { isoYear: 2026, week: 53 }],
    ["2026-12-31T00:00:00", { isoYear: 2026, week: 53 }],
    ["2027-01-01T00:00:00", { isoYear: 2026, week: 53 }],
    ["2025-12-29T00:00:00", { isoYear: 2026, week: 1 }],
  ];
  for (const [iso, expected] of cases) {
    it(`${iso} -> isoYear ${expected.isoYear}, week ${expected.week}`, () => {
      expect(isoWeekInfo(new Date(iso))).toEqual(expected);
    });
  }
});

describe("mastheadVolume", () => {
  it("zero-pads the week to 2 digits", () => {
    expect(mastheadVolume(new Date("2026-01-01T00:00:00").getTime())).toBe(
      "Vol. 2026 · No. 01",
    );
  });

  it("renders a double-digit week without extra padding", () => {
    expect(mastheadVolume(new Date("2026-12-31T00:00:00").getTime())).toBe(
      "Vol. 2026 · No. 53",
    );
  });
});

describe("censusLine", () => {
  it("omits the 'of M' clause when docCount matches rows.length", () => {
    const rows = [doc({ id: "a", word_count: 100 }), doc({ id: "b", word_count: 50 })];
    expect(censusLine(rows, 2)).toBe("2 artifacts · 150 words");
  });

  it("shows 'of M' when docCount differs (a filtered subset)", () => {
    const rows = [doc({ id: "a", word_count: 100 })];
    expect(censusLine(rows, 40)).toBe("1 of 40 artifacts · 100 words");
  });

  it("singularizes artifact/word at exactly 1", () => {
    const rows = [doc({ id: "a", word_count: 1 })];
    expect(censusLine(rows)).toBe("1 artifact · 1 word");
  });

  it("handles zero rows", () => {
    expect(censusLine([])).toBe("0 artifacts · 0 words");
  });
});
