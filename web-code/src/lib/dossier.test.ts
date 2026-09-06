// V72-G1.2 — the dossier's pure half, walked against the RUST GOLDEN.
//
// `crates/kb-code-server/tests/fixtures/entity-dossier.golden.json` is the
// fixture `entities/dossier.rs`'s own test asserts against. Reading it from
// here is the same cross-crate discipline `commands/registry.gen.test.ts` and
// `lib/kbcq.golden.test.ts` already use: a TEST may reach across the crate
// boundary (vitest runs from the repo), the BUNDLE may not — so a field
// renamed on the wire fails HERE, by name, instead of surfacing as an
// `undefined` on screen.

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { DossierHonesty, DossierOut, UsageGroup } from "../api/types";
import {
  cycleMemberSort,
  DEFAULT_MEMBER_SORT,
  DOSSIER_SECTIONS,
  droppedLanes,
  droppedTotal,
  honestyCaption,
  MEMBER_SORTS,
  nextUsagesPerKind,
  readStateOf,
  sortMembers,
  stepSection,
  usageGroupCensus,
  visibilityRank,
  MAX_USAGES_PER_KIND,
} from "./dossier";

const GOLDEN = fileURLToPath(
  new URL(
    "../../../crates/kb-code-server/tests/fixtures/entity-dossier.golden.json",
    import.meta.url,
  ),
);

function golden(): DossierOut {
  return JSON.parse(readFileSync(GOLDEN, "utf8")) as DossierOut;
}

describe("the entity/1 golden types-check against this SPA's mirror", () => {
  it("carries every field the renderer reads", () => {
    const d = golden();
    expect(d.schema).toBe("entity/1");
    expect(d.entity.fqn).toBe("Shop::Order");
    expect(d.entity.kind).toBe("class");
    expect(d.entity.trust_counts).toEqual({ exact: 2, likely: 0, candidate: 0 });
    // Each lane is present and non-degenerate, so a renderer bug cannot hide
    // behind an empty fixture.
    expect(d.definitions.length).toBeGreaterThan(0);
    expect(d.members.length).toBeGreaterThan(0);
    expect(d.usages.groups.length).toBeGreaterThan(0);
    expect(d.unknown_members.length).toBeGreaterThan(0);
    expect(d.namespace_tree.length).toBeGreaterThan(0);
    expect(d.honesty.budget.order.length).toBeGreaterThan(0);
    for (const b of d.definitions) {
      expect(typeof b.opener).toBe("string");
      expect(typeof b.opener_form).toBe("string");
      expect(typeof b.reopening_index).toBe("number");
      expect(["exact", "likely", "candidate"]).toContain(b.trust);
    }
    for (const m of d.members) {
      expect(["exact", "likely", "candidate"]).toContain(m.trust);
      expect(["tree", "macro", "assignment"]).toContain(m.via);
    }
  });

  it("the members arrive in the SERVER's visibility order — this side's rank mirrors ruby_body::visibility_rank", () => {
    const d = golden();
    // The default sort is a no-op re-ordering of what arrived: if this side's
    // rank disagreed with the Rust's, sorting the response would REORDER it,
    // and that is exactly the drift this assertion catches.
    const resorted = sortMembers(d.members, "visibility");
    expect(resorted.map((m) => `${m.name}/${m.visibility}`)).toEqual(
      d.members.map((m) => `${m.name}/${m.visibility}`),
    );
  });

  it("`unknown` visibility sorts LAST, never into the public block", () => {
    expect(visibilityRank("public")).toBeLessThan(visibilityRank("protected"));
    expect(visibilityRank("protected")).toBeLessThan(visibilityRank("private"));
    expect(visibilityRank("private")).toBeLessThan(visibilityRank("module_function"));
    expect(visibilityRank("module_function")).toBeLessThan(visibilityRank("unknown"));
    // Total over an unmodelled value — a wire that grows a sixth visibility
    // must not throw here, and must not be ranked above a known one.
    expect(visibilityRank("something-new")).toBe(visibilityRank("unknown"));
  });
});

describe("the member table's sort is a VIEW, never a filter", () => {
  it("every sort returns exactly the same rows", () => {
    const d = golden();
    const key = (m: { name: string; path: string; line: number }) => `${m.name}@${m.path}:${m.line}`;
    const base = d.members.map(key).sort();
    for (const s of MEMBER_SORTS) {
      expect(sortMembers(d.members, s).map(key).sort()).toEqual(base);
    }
  });

  it("sorts by name and by defining type as asked, and is total on junk", () => {
    const d = golden();
    // Code-unit order, mirroring Rust's `str::cmp` — NOT `localeCompare`.
    const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
    const byName = sortMembers(d.members, "name").map((m) => m.name);
    expect([...byName].sort(cmp)).toEqual(byName);
    const byType = sortMembers(d.members, "defining_type").map((m) => m.defining_type);
    expect([...byType].sort(cmp)).toEqual(byType);
    // An unrecognised key falls back to the default rather than throwing.
    expect(sortMembers(d.members, "nonsense" as never).length).toBe(d.members.length);
  });

  it("the sort CYCLES and restarts from a junk value", () => {
    expect(cycleMemberSort("visibility")).toBe("name");
    expect(cycleMemberSort("name")).toBe("defining_type");
    expect(cycleMemberSort("defining_type")).toBe("visibility");
    expect(cycleMemberSort("who-knows")).toBe(DEFAULT_MEMBER_SORT);
  });

  it("does not mutate the response's own array", () => {
    const d = golden();
    const before = d.members.map((m) => m.name);
    sortMembers(d.members, "name");
    expect(d.members.map((m) => m.name)).toEqual(before);
  });
});

describe("the section spine", () => {
  it("is D6's order, and every section has a unique DOM id", () => {
    expect(DOSSIER_SECTIONS.map((s) => s.id)).toEqual([
      "definitions",
      "members",
      "hierarchy",
      "usages",
      "unknown-members",
      "namespace",
    ]);
    expect(new Set(DOSSIER_SECTIONS.map((s) => s.domId)).size).toBe(DOSSIER_SECTIONS.length);
  });

  it("`]s`/`[s` wrap and are total on an unknown cursor", () => {
    expect(stepSection("definitions", 1)).toBe("members");
    expect(stepSection("namespace", 1)).toBe("definitions");
    expect(stepSection("definitions", -1)).toBe("namespace");
    expect(stepSection(null, 1)).toBe("definitions");
    expect(stepSection(null, -1)).toBe("namespace");
    expect(stepSection("gone" as never, 1)).toBe("definitions");
  });
});

describe("the four read states each have their own answer", () => {
  const d = golden();

  it("loading, error, ok", () => {
    expect(readStateOf({ isLoading: true, error: null, data: undefined })).toBe("loading");
    // An error WINS over a stale success — a page that failed to refresh must
    // not render as though it succeeded.
    expect(readStateOf({ isLoading: false, error: new Error("boom"), data: d })).toBe("error");
    expect(readStateOf({ isLoading: false, error: null, data: undefined })).toBe("loading");
    expect(readStateOf({ isLoading: false, error: null, data: d })).toBe("ok");
  });

  it("the server's own `empty` and `partial` are surfaced, not flattened into ok", () => {
    const empty: DossierOut = { ...d, honesty: { ...d.honesty, state: "empty", reason: "nothing" } };
    const partial: DossierOut = { ...d, honesty: { ...d.honesty, state: "partial", reason: "cut" } };
    expect(readStateOf({ isLoading: false, error: null, data: empty })).toBe("empty");
    expect(readStateOf({ isLoading: false, error: null, data: partial })).toBe("partial");
  });
});

describe("the honesty caption names what was dropped, from the wire's own numbers", () => {
  const base = golden().honesty;

  it("`ok` earns no banner — that is what makes a banner mean something", () => {
    expect(honestyCaption(base)).toBeNull();
  });

  it("`empty` renders its reason, and never an empty string", () => {
    expect(honestyCaption({ ...base, state: "empty", reason: "no such entity" })).toContain(
      "no such entity",
    );
    const noReason = honestyCaption({ ...base, state: "empty", reason: undefined });
    expect(noReason).toBeTruthy();
    expect(noReason).toContain("no reason given");
  });

  it("`partial` names the budget AND every lane that lost rows, in the server's spend order", () => {
    const h: DossierHonesty = {
      ...base,
      state: "partial",
      reason: "some lane is incomplete",
      budget: {
        ...base.budget,
        requested: 3,
        spent: 3,
        dropped: {
          definitions: 0,
          members: 5,
          ancestors: 0,
          mixins: 0,
          descendants: 1,
          implementors: 0,
          unknown_members: 1,
          namespace_tree: 0,
          usages: 2,
        },
      },
    };
    const cap = honestyCaption(h)!;
    expect(cap).toContain("budget 3 of 3 rows");
    // The order is `budget.order`'s, not object-key order — members before
    // unknown_members before descendants before usages.
    expect(cap).toContain("dropped 5 members, 1 unknown_members, 1 descendants, 2 usages");
    expect(cap.indexOf("5 members")).toBeLessThan(cap.indexOf("2 usages"));
    // Zero-count lanes are never named — they would bury the ones that lost.
    expect(cap).not.toContain("definitions");
    expect(cap).not.toContain("mixins");
  });

  it("a `partial` raised for a NON-budget reason still states the budget, and does not invent drops", () => {
    const cap = honestyCaption({ ...base, state: "partial", reason: "zeitwerk read degraded" })!;
    expect(cap).toContain("zeitwerk read degraded");
    expect(cap).toContain("budget");
    expect(cap).not.toContain("dropped");
  });

  it("droppedLanes skips zeroes and droppedTotal sums the wire's numbers", () => {
    const dropped = {
      definitions: 0,
      members: 5,
      ancestors: 0,
      mixins: 0,
      descendants: 1,
      implementors: 0,
      unknown_members: 1,
      namespace_tree: 0,
      usages: 2,
    };
    const lanes = droppedLanes({ ...base, budget: { ...base.budget, dropped } });
    expect(lanes.map((l) => l.lane)).toEqual(["members", "unknown_members", "descendants", "usages"]);
    expect(droppedTotal(dropped)).toBe(9);
    expect(droppedTotal(base.budget.dropped)).toBe(0);
  });

  it("a lane the server's `order` does not mention is still reported, never silently lost", () => {
    const dropped = { ...base.budget.dropped, members: 2 } as unknown as Record<string, number>;
    (dropped as Record<string, number>)["a_future_lane"] = 4;
    const lanes = droppedLanes({
      ...base,
      budget: { ...base.budget, dropped: dropped as never },
    });
    expect(lanes.map((l) => l.lane)).toContain("a_future_lane");
    expect(lanes.find((l) => l.lane === "a_future_lane")!.n).toBe(4);
  });
});

describe("usage counts come from the wire, never from rows.length", () => {
  const d = golden();

  it("a TRUNCATED group reports the true total and says how many are shown", () => {
    const g = d.usages.groups.find((x) => x.truncated)!;
    expect(g).toBeDefined();
    // The golden's own shape: more `total` than delivered `rows`.
    expect(g.total).toBeGreaterThan(g.rows.length);
    const c = usageGroupCensus(g);
    expect(c.total).toBe(g.total);
    expect(c.shown).toBe(g.rows.length);
    expect(c.caption).toContain(`${g.rows.length} of ${g.total} shown`);
    // The census is the WIRE's, and its basis is stated rather than assumed.
    expect(c.census).toEqual(g.trust_census);
    expect(c.caption).toContain(g.census_basis);
  });

  it("an UNTRUNCATED group's headline count is still the wire's `total`", () => {
    const g: UsageGroup = {
      kind: "call",
      total: 3,
      truncated: false,
      trust_census: { exact: 1, likely: 2, candidate: 0 },
      census_basis: "returned",
      rows: [],
    };
    const c = usageGroupCensus(g);
    // `rows` is EMPTY and the caption still says 3 — the wire is the source,
    // and this is precisely the assertion that fails if someone "fixes" the
    // renderer to count what it holds.
    expect(c.caption).toContain("3 total");
    expect(c.total).toBe(3);
  });

  it("`show more` re-asks with a bigger cut and vanishes at the cap", () => {
    expect(nextUsagesPerKind(20)).toBe(40);
    expect(nextUsagesPerKind(150)).toBe(MAX_USAGES_PER_KIND);
    expect(nextUsagesPerKind(MAX_USAGES_PER_KIND)).toBeNull();
    expect(nextUsagesPerKind(MAX_USAGES_PER_KIND + 50)).toBeNull();
  });
});
