import { describe, expect, it } from "vitest";
import {
  COLUMN_IDS,
  COLUMN_NAMES,
  GENERAL_LANE,
  attentionCount,
  columnsOf,
  lanesOf,
  moveCursor,
  nowRows,
  orderCards,
  orderSlates,
  readingOrder,
  seenChipFor,
  seenLabel,
  seenTitle,
  totalAttention,
  warnRows,
  SEEN_KINDS,
} from "./slateLanes";
import type {
  SlateBoardCard,
  SlateBoardSections,
  SlateCounts,
  SlateKind,
  SlateSummary,
} from "../api/slateTypes";

function card(
  seq: number,
  over: Partial<SlateBoardCard> & { kind?: SlateKind } = {},
): SlateBoardCard {
  return {
    seq,
    id: `e_${seq}`,
    kind: over.kind ?? "found",
    line: `line ${seq}`,
    who: {
      origin: "agent",
      harness: "claude",
      session_short: "4b7e",
      tag: "claude/4b7e",
    },
    age_secs: 60,
    tier: "folded",
    marks: 0,
    pinned: false,
    contested: false,
    body: null,
    marks_by: [],
    has_sketch: false,
    ...over,
  } as SlateBoardCard;
}

const EMPTY_SECTIONS: SlateBoardSections = {
  now: [],
  warn: [],
  hand: [],
  ask: [],
  take: [],
  found_idea: [],
  tried: [],
};

function counts(over: Partial<SlateCounts> = {}): SlateCounts {
  return {
    now: 0,
    warn: 0,
    hand_unack: 0,
    ask_open: 0,
    take_live: 0,
    take_stale: 0,
    take_contested: 0,
    found: 0,
    idea: 0,
    tried: 0,
    ...over,
  };
}

describe("orderCards — pinned, then marks descending, then newest first", () => {
  it("is the digest's own order", () => {
    const cards = [
      card(10),
      card(20, { marks: 3 }),
      card(30, { pinned: true }),
      card(40, { marks: 1 }),
      card(50),
      card(60, { pinned: true, marks: 9 }),
    ];
    expect(orderCards(cards).map((c) => c.seq)).toEqual([60, 30, 20, 40, 50, 10]);
  });

  it("puts a pinned card first even with zero marks and the oldest seq", () => {
    const cards = [card(99, { marks: 5 }), card(1, { pinned: true })];
    expect(orderCards(cards)[0].seq).toBe(1);
  });

  it("never mutates its input", () => {
    const cards = [card(1), card(2)];
    const copy = cards.map((c) => c.seq);
    orderCards(cards);
    expect(cards.map((c) => c.seq)).toEqual(copy);
  });

  it("is total on an empty list", () => {
    expect(orderCards([])).toEqual([]);
  });
});

describe("columnsOf — five columns, named, always present", () => {
  it("emits exactly the five section columns in reading order", () => {
    const cols = columnsOf(EMPTY_SECTIONS);
    expect(cols.map((c) => c.id)).toEqual([
      "hand",
      "ask",
      "take",
      "found_idea",
      "tried",
    ]);
    expect(cols.map((c) => c.name)).toEqual(COLUMN_IDS.map((id) => COLUMN_NAMES[id]));
  });

  it("renders an EMPTY column rather than dropping it (the layout must not move)", () => {
    const cols = columnsOf(undefined);
    expect(cols).toHaveLength(5);
    expect(cols.every((c) => c.cards.length === 0)).toBe(true);
  });

  it("orders each column's cards", () => {
    const cols = columnsOf({
      ...EMPTY_SECTIONS,
      found_idea: [card(1), card(2, { pinned: true }), card(3, { marks: 2 })],
    });
    const fi = cols.find((c) => c.id === "found_idea")!;
    expect(fi.cards.map((c) => c.seq)).toEqual([2, 3, 1]);
  });

  it("NOW and WARN are the band, never a column", () => {
    expect(COLUMN_IDS).not.toContain("now" as never);
    expect(COLUMN_IDS).not.toContain("warn" as never);
  });
});

describe("lanesOf — swimlanes, general lane included", () => {
  const sections: SlateBoardSections = {
    ...EMPTY_SECTIONS,
    found_idea: [
      card(1, { topic: "v7" }),
      card(2, { topic: "perf" }),
      card(3),
      card(4, { topic: "v7" }),
    ],
    ask: [card(5, { kind: "ask", topic: "perf" })],
  };

  it("makes one lane per topic in the board's topic order, general LAST", () => {
    const lanes = lanesOf(sections, ["v7", "perf"]);
    expect(lanes.map((l) => l.label)).toEqual(["v7", "perf", GENERAL_LANE]);
    expect(lanes[lanes.length - 1].topic).toBeNull();
  });

  it("gives every lane the same five columns, filtered to its own cards", () => {
    const lanes = lanesOf(sections, ["v7", "perf"]);
    const v7 = lanes[0];
    expect(v7.columns.map((c) => c.id)).toEqual([...COLUMN_IDS]);
    expect(v7.columns.find((c) => c.id === "found_idea")!.cards.map((c) => c.seq)).toEqual([4, 1]);
    expect(v7.total).toBe(2);
    const perf = lanes[1];
    expect(perf.total).toBe(2);
  });

  it("picks up a topic that only exists on a card, after the reported ones", () => {
    const lanes = lanesOf(
      { ...EMPTY_SECTIONS, found_idea: [card(1, { topic: "ghost" })] },
      ["v7"],
    );
    expect(lanes.map((l) => l.label)).toEqual(["v7", "ghost"]);
  });

  it("drops an EMPTY general lane, but always renders at least one lane", () => {
    const lanes = lanesOf({ ...EMPTY_SECTIONS, found_idea: [card(1, { topic: "v7" })] }, ["v7"]);
    expect(lanes.map((l) => l.label)).toEqual(["v7"]);
    expect(lanesOf(EMPTY_SECTIONS, []).map((l) => l.label)).toEqual([GENERAL_LANE]);
  });
});

describe("nowRows / warnRows — the band", () => {
  it("orders NOW rows by the board's topic order with general last", () => {
    const rows = nowRows(
      {
        ...EMPTY_SECTIONS,
        now: [
          card(10, { kind: "now" }),
          card(20, { kind: "now", topic: "perf" }),
          card(30, { kind: "now", topic: "v7" }),
        ],
      },
      ["v7", "perf"],
    );
    expect(rows.map((r) => r.label)).toEqual(["v7", "perf", GENERAL_LANE]);
  });

  it("labels an untopicked NOW with the general lane's dash", () => {
    const rows = nowRows({ ...EMPTY_SECTIONS, now: [card(1, { kind: "now" })] });
    expect(rows[0].label).toBe(GENERAL_LANE);
    expect(rows[0].topic).toBeNull();
  });

  it("returns warn rows ordered and never truncated (a warn never ages out)", () => {
    const warns = warnRows({
      ...EMPTY_SECTIONS,
      warn: [card(1, { kind: "warn" }), card(2, { kind: "warn", pinned: true })],
    });
    expect(warns.map((c) => c.seq)).toEqual([2, 1]);
  });

  it("is total on undefined sections", () => {
    expect(nowRows(undefined)).toEqual([]);
    expect(warnRows(undefined)).toEqual([]);
  });
});

describe("attentionCount — the four terms, and only the four", () => {
  it("sums unacked hands + open asks + contested takes + stale takes", () => {
    expect(
      attentionCount(
        counts({ hand_unack: 2, ask_open: 3, take_contested: 1, take_stale: 4 }),
      ),
    ).toBe(10);
  });

  it("ignores every OTHER count — a busy board with nothing to attend to is 0", () => {
    expect(
      attentionCount(
        counts({ now: 9, warn: 9, take_live: 9, found: 9, idea: 9, tried: 9 }),
      ),
    ).toBe(0);
  });

  it("is 0 (never null, never NaN) when the counts are missing", () => {
    expect(attentionCount(undefined)).toBe(0);
    expect(attentionCount({} as SlateCounts)).toBe(0);
  });

  it("sums fleet-wide across slates, and is 0 while the list is loading", () => {
    const slates = [
      { counts: counts({ hand_unack: 1 }) },
      { counts: counts({ ask_open: 2, take_stale: 1 }) },
    ] as SlateSummary[];
    expect(totalAttention(slates)).toBe(4);
    expect(totalAttention(undefined)).toBe(0);
    expect(totalAttention([])).toBe(0);
  });
});

// The "chip never flashes 0" rule, as a fact about the DATA rather than
// about a render: the count is 0 both while the list is loading and when
// there is nothing to attend to, so a `> 0` guard at the render site cannot
// show a zero in either state — there is no third value it could show.
describe("the attention chip never flashes a 0", () => {
  const hidden = (n: number) => !(n > 0);
  it("hides while loading, hides at zero, shows only a real count", () => {
    expect(hidden(totalAttention(undefined))).toBe(true);
    expect(hidden(totalAttention([]))).toBe(true);
    expect(
      hidden(totalAttention([{ counts: counts() }] as SlateSummary[])),
    ).toBe(true);
    expect(
      hidden(totalAttention([{ counts: counts({ ask_open: 1 }) }] as SlateSummary[])),
    ).toBe(false);
  });
});

describe("orderSlates — attention first, closed last", () => {
  const s = (slug: string, over: Partial<SlateSummary> = {}): SlateSummary =>
    ({
      slug,
      head_seq: 1,
      generation: 1,
      updated_unix: 0,
      closed: false,
      topics: [],
      counts: counts(),
      ...over,
    }) as SlateSummary;

  it("ranks by attention, then recency, then slug", () => {
    const rows = orderSlates([
      s("c", { updated_unix: 10 }),
      s("a", { counts: counts({ ask_open: 2 }) }),
      s("b", { updated_unix: 20 }),
      s("d", { counts: counts({ hand_unack: 5 }) }),
    ]);
    expect(rows.map((r) => r.slug)).toEqual(["d", "a", "b", "c"]);
  });

  it("sinks a closed slate below every open one, however busy", () => {
    const rows = orderSlates([
      s("closed", { closed: true, counts: counts({ ask_open: 99 }) }),
      s("open"),
    ]);
    expect(rows.map((r) => r.slug)).toEqual(["open", "closed"]);
  });

  it("is total on undefined", () => {
    expect(orderSlates(undefined)).toEqual([]);
  });
});

describe("readingOrder + moveCursor — the j/k walk", () => {
  const sections: SlateBoardSections = {
    ...EMPTY_SECTIONS,
    now: [card(1, { kind: "now" })],
    warn: [card(2, { kind: "warn" })],
    hand: [card(3, { kind: "hand" })],
    ask: [card(4, { kind: "ask" })],
    take: [card(5, { kind: "take" })],
    found_idea: [card(6)],
    tried: [card(7, { kind: "tried" })],
  };

  it("walks the band then each column left to right", () => {
    expect(readingOrder(sections, []).map((c) => c.seq)).toEqual([1, 2, 3, 4, 5, 6, 7]);
  });

  it("covers every card exactly once in swimlane mode too", () => {
    const swim = readingOrder(sections, [], true).map((c) => c.seq).sort((a, b) => a - b);
    expect(swim).toEqual([1, 2, 3, 4, 5, 6, 7]);
  });

  it("clamps at both ends instead of wrapping", () => {
    const order = readingOrder(sections, []);
    expect(moveCursor(order, null, 1)).toBe(1);
    expect(moveCursor(order, null, -1)).toBe(7);
    expect(moveCursor(order, 1, -1)).toBe(1);
    expect(moveCursor(order, 7, 1)).toBe(7);
    expect(moveCursor(order, 4, 1)).toBe(5);
    expect(moveCursor(order, 4, -1)).toBe(3);
  });

  it("recovers to the first card when the cursor's card has left the board", () => {
    expect(moveCursor(readingOrder(sections, []), 999, 1)).toBe(1);
  });

  it("is null on an empty board", () => {
    expect(moveCursor([], null, 1)).toBeNull();
    expect(moveCursor([], 3, 1)).toBeNull();
  });
});

// ── D27 · seen chips ─────────────────────────────────────────────────────

describe("seenLabel", () => {
  it("counts the served sessions", () => {
    expect(seenLabel(["codex/8f2a"])).toBe("seen by 1");
    expect(seenLabel(["codex/8f2a", "claude/4b7e"])).toBe("seen by 2");
  });

  it("never flashes a confident zero", () => {
    expect(seenLabel([])).toBeNull();
    expect(seenLabel(undefined)).toBeNull();
    expect(seenLabel(null)).toBeNull();
  });

  it("says SERVED on hover — a cursor is attribution, not a read receipt", () => {
    expect(seenTitle(["codex/8f2a", "claude/4b7e"])).toBe(
      "served to codex/8f2a, claude/4b7e",
    );
    expect(seenTitle([])).toBeUndefined();
  });
});

describe("seenChipFor", () => {
  const seen = ["codex/8f2a", "claude/4b7e"];

  it("chips a whole-tier hand with the count and the hover list", () => {
    expect(
      seenChipFor(card(9, { kind: "hand", tier: "whole", seen_by: seen })),
    ).toEqual({ label: "seen by 2", title: "served to codex/8f2a, claude/4b7e" });
  });

  it("chips exactly the three kinds the digest chips (never a WARN)", () => {
    // kb-core's `item_text` gates on NOW/HAND/ASK at whole tier
    // (`seen_by_renders_on_three_kinds_at_whole_tier_only`); the board is a
    // second presenter over the same projection, so it gates identically.
    expect([...SEEN_KINDS]).toEqual(["now", "hand", "ask"]);
    for (const kind of SEEN_KINDS) {
      expect(
        seenChipFor(card(1, { kind, tier: "whole", seen_by: seen })),
        kind,
      ).not.toBeNull();
    }
    for (const kind of ["warn", "take", "found", "idea", "tried"] as const) {
      expect(
        seenChipFor(card(1, { kind, tier: "whole", seen_by: seen })),
        kind,
      ).toBeNull();
    }
  });

  it("never chips a folded card, and never an unserved one", () => {
    expect(
      seenChipFor(card(2, { kind: "ask", tier: "folded", seen_by: seen })),
    ).toBeNull();
    expect(seenChipFor(card(3, { kind: "ask", tier: "whole", seen_by: [] }))).toBeNull();
    expect(seenChipFor(card(4, { kind: "ask", tier: "whole" }))).toBeNull();
  });
});

describe("sessions_served is not a rank term", () => {
  const row = (slug: string, served: number | undefined): SlateSummary =>
    ({
      slug,
      head_seq: 1,
      generation: 1,
      updated_unix: 0,
      closed: false,
      topics: [],
      counts: counts({ hand_unack: 1 }),
      sessions_served: served,
    }) as SlateSummary;

  it("orderSlates ignores it entirely (attention/recency/slug only)", () => {
    expect(orderSlates([row("a", 0), row("b", 99)]).map((r) => r.slug)).toEqual([
      "a",
      "b",
    ]);
    expect(
      orderSlates([row("b", 99), row("a", undefined)]).map((r) => r.slug),
    ).toEqual(["a", "b"]);
  });
});
