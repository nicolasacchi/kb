import { describe, expect, it } from "vitest";
import type { DocSummary, EchoOut, ResurfaceItem } from "../api/client";
import { composePage } from "./broadsheet";
import {
  collapsedKey,
  composeFrontPage,
  countNewSince,
  echoAge,
  echoTarget,
  editionLine,
  fleetKbs,
  FLEET_KB_CAP,
  mastheadDate,
  parseVisit,
  pickIndexHero,
  rotateVisit,
  rowKey,
  showsFrontPage,
  visitKey,
  type FrontPageGate,
  type FrontRow,
} from "./frontpage";

const NOW = Date.UTC(2026, 9, 16, 12, 0, 0);
const NOW_S = Math.floor(NOW / 1000);
const DAY = 86_400;

function doc(partial: Partial<DocSummary> & { id: string }): DocSummary {
  return {
    title: partial.id,
    path: `${partial.id}.html`,
    folder: "",
    source_relative: `${partial.id}.html`,
    ...partial,
  };
}
const row = (kb: string, d: DocSummary): FrontRow => ({ kb, doc: d });

function item(id: string, score: number, reasons: string[] = ["2 open comments"]): ResurfaceItem {
  return {
    id,
    title: `T ${id}`,
    source_relative: `${id}.html`,
    score,
    comment_term: 0,
    read_term: 0,
    open_comments: 0,
    reasons,
  } as ResurfaceItem;
}

const OPEN: FrontPageGate = {
  activeFolder: null,
  tagCount: 0,
  capCount: 0,
  q: "",
  category: "",
  readFilter: null,
  since: "all",
  fromUnix: null,
  toUnix: null,
  sort: "recent",
  view: "grid",
};

describe("showsFrontPage (the unfiltered-Library predicate)", () => {
  it("shows on the bare grid and list", () => {
    expect(showsFrontPage(OPEN)).toBe(true);
    expect(showsFrontPage({ ...OPEN, view: "list" })).toBe(true);
  });
  it.each([
    ["folder", { activeFolder: "pm" }],
    ["tag", { tagCount: 1 }],
    ["cap", { capCount: 1 }],
    ["q", { q: "x" }],
    ["category", { category: "note" }],
    ["read", { readFilter: ["unread"] }],
    ["since", { since: "7d" }],
    ["from", { fromUnix: 1 }],
    ["to", { toUnix: 1 }],
    ["sort", { sort: "title" }],
    ["press view", { view: "press" }],
    ["atlas view", { view: "atlas" }],
  ] as [string, Partial<FrontPageGate>][])("hides on %s", (_n, patch) => {
    expect(showsFrontPage({ ...OPEN, ...patch })).toBe(false);
  });
  it("hides for the visual-golden URL (folder + title sort)", () => {
    expect(showsFrontPage({ ...OPEN, activeFolder: "pm", sort: "title" })).toBe(false);
  });
});

describe("fleetKbs", () => {
  it("caps at the daemon fan-out and skips memory corpora", () => {
    const kbs = [
      { name: "mem", memory_scope: "global" },
      ...Array.from({ length: 12 }, (_, i) => ({ name: `k${i}`, memory_scope: null })),
    ];
    const out = fleetKbs(kbs);
    expect(out).toHaveLength(FLEET_KB_CAP);
    expect(out[0]).toBe("k0");
    expect(out).not.toContain("mem");
  });
});

describe("rotateVisit", () => {
  it("first mount of a session moves last into prev and stamps now", () => {
    const r = rotateVisit({ last: 100, prev: 50 }, false, 200);
    expect(r.state).toEqual({ last: 200, prev: 100 });
    expect(r.since).toBe(100);
  });
  it("later mounts in the same session leave the record alone", () => {
    const st = { last: 200, prev: 100 };
    const r = rotateVisit(st, true, 300);
    expect(r.state).toBe(st);
    expect(r.since).toBe(100);
  });
  it("a first-ever visit has no earlier visit on record", () => {
    expect(rotateVisit({ last: null, prev: null }, false, 200).since).toBeNull();
  });
  it("parseVisit is total", () => {
    expect(parseVisit(null)).toEqual({ last: null, prev: null });
    expect(parseVisit("not json")).toEqual({ last: null, prev: null });
    expect(parseVisit('{"last":"x","prev":3}')).toEqual({ last: null, prev: 3 });
  });
  it("keys are per scope", () => {
    expect(visitKey("a")).not.toBe(visitKey(null));
    expect(collapsedKey("a")).not.toBe(collapsedKey("b"));
  });
});

describe("countNewSince", () => {
  const rows = [doc({ id: "a", mtime_unix: 300 }), doc({ id: "b", mtime_unix: 100 })];
  it("is null with no earlier visit", () => {
    expect(countNewSince([{ rows, hasMore: false }], null)).toBeNull();
  });
  it("is exact when an older row bounds the page", () => {
    expect(countNewSince([{ rows, hasMore: true }], 200)).toEqual({ count: 1, exact: true });
  });
  it("is a lower bound when every loaded row is new and more exist", () => {
    expect(countNewSince([{ rows, hasMore: true }], 50)).toEqual({ count: 2, exact: false });
    expect(countNewSince([{ rows, hasMore: false }], 50)).toEqual({ count: 2, exact: true });
  });
});

describe("editionLine / mastheadDate", () => {
  it("states only what is true", () => {
    expect(editionLine({ total: 12, words: 4200, newSince: { count: 3, exact: true } })).toBe(
      "12 artifacts · 4,200 words · 3 new since your last visit",
    );
    expect(editionLine({ total: 1, words: null, newSince: null })).toBe(
      "1 artifact · no earlier visit on record here",
    );
    expect(editionLine({ total: 9, words: null, newSince: { count: 5, exact: false } })).toContain(
      "5+ new",
    );
  });
  it("names the corpora, and says when it saw only the first few", () => {
    expect(editionLine({ total: 9, corpora: 3, words: null, newSince: null })).toContain(
      "across 3 corpora",
    );
    expect(
      editionLine({ total: 9, corpora: 8, corporaCapped: true, words: null, newSince: null }),
    ).toContain("across the first 8 corpora");
  });
  it("spells the date from fixed tables", () => {
    expect(mastheadDate(new Date(2026, 9, 16, 12).getTime())).toBe("Friday 16 October 2026");
  });
});

describe("composeFrontPage", () => {
  const rows: FrontRow[] = [
    row("k", doc({ id: "old", word_count: 5000, mtime_unix: NOW_S - 200 * DAY })),
    row("k", doc({ id: "n1", word_count: 800, mtime_unix: NOW_S - 1 * DAY })),
    row("k", doc({ id: "n2", word_count: 600, mtime_unix: NOW_S - 2 * DAY })),
    row("k", doc({ id: "n3", word_count: 100, mtime_unix: NOW_S - 3 * DAY })),
  ];
  const base = { rows, openMass: new Map<string, number>(), resurface: [], nowMs: NOW };

  it("leads with the top resurface item, its reason as the dateline", () => {
    const m = composeFrontPage({
      ...base,
      since: null,
      resurface: [
        { kb: "k", item: item("n2", 1, ["lowest"]) },
        { kb: "k", item: item("n3", 9, ["3 open comments · oldest 4d"]) },
      ],
    });
    expect(m.lead?.id).toBe("n3");
    expect(m.lead?.from).toBe("resurface");
    expect(m.lead?.dateline).toBe("3 open comments · oldest 4d");
    expect(m.lead?.row?.id).toBe("n3");
    expect(m.features.map((f) => f.doc.id)).not.toContain("n3");
  });

  it("falls back to the broadsheet's own lead when the queue is empty", () => {
    const m = composeFrontPage({ ...base, since: null });
    const bs = composePage(
      rows.map((r) => r.doc),
      new Map(),
      NOW,
    );
    expect(m.lead?.from).toBe("ranked");
    expect(m.lead?.id).toBe(bs.lead?.doc.id);
    expect(m.features.map((f) => f.doc.id)).toEqual(bs.features.map((d) => d.id));
  });

  it("features are the new-since-visit items when at least two exist", () => {
    const m = composeFrontPage({ ...base, since: NOW_S - 3.5 * DAY });
    expect(m.featuresKind).toBe("new");
    expect(m.features.every((f) => (f.doc.mtime_unix ?? 0) > NOW_S - 3.5 * DAY)).toBe(true);
  });

  it("falls back to ranked features when fewer than two are new", () => {
    const m = composeFrontPage({ ...base, since: NOW_S - 1.5 * DAY });
    expect(m.featuresKind).toBe("ranked");
  });

  it("keys comment mass on (kb, id), so equal ids in two corpora do not merge", () => {
    const two: FrontRow[] = [
      row("a", doc({ id: "x", word_count: 100, mtime_unix: NOW_S })),
      row("b", doc({ id: "x", word_count: 100, mtime_unix: NOW_S })),
    ];
    const m = composeFrontPage({
      rows: two,
      openMass: new Map([[rowKey("b", "x"), 4]]),
      resurface: [],
      since: null,
      nowMs: NOW,
    });
    expect(m.lead?.kb).toBe("b");
  });

  it("is empty-safe", () => {
    const m = composeFrontPage({ ...base, rows: [], since: null });
    expect(m.lead).toBeNull();
    expect(m.features).toEqual([]);
  });
});

describe("echoes + index hero", () => {
  const href = (kb: string, rel: string) => `/a/${kb}/${rel}`;
  it("ages", () => {
    expect(echoAge(12)).toBe("1y");
    expect(echoAge(6)).toBe("6mo");
  });
  it("targets", () => {
    const art = { kind: "created", months_ago: 12, title: "t", detail: "", source_relative: "a/b.html" } as EchoOut;
    expect(echoTarget("k", art, href)).toBe("/a/k/a/b.html");
    const ses = { kind: "worked", months_ago: 12, title: "t", detail: "", session_id: "s 1" } as EchoOut;
    expect(echoTarget("k", ses, href)).toBe("/sessions?kb=k&focus=s%201");
    expect(echoTarget("k", { ...ses, session_id: undefined } as EchoOut, href)).toBeNull();
  });
  it("pickIndexHero prefers the root index.html", () => {
    const deep = doc({ id: "d", path: "sub/index.html", mtime_unix: 900 });
    const root = doc({ id: "r", path: "index.html", mtime_unix: 10 });
    expect(pickIndexHero([deep, root])?.id).toBe("r");
    expect(pickIndexHero([deep])?.id).toBe("d");
    expect(pickIndexHero([doc({ id: "z" })])).toBeNull();
  });
});
