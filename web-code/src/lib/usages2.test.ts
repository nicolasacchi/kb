import { describe, expect, it } from "vitest";
import type { Usages2Out, UsageRow2 } from "../api/types";
import {
  applyChips,
  censusOf,
  chipsAreEmpty,
  GROUP_AXES,
  groupRows,
  NO_CHIPS,
  ROLE_TEST,
  ROLE_VENDOR,
  stepCursor,
  TRUST_ORDER,
  usagesTitle,
  walkOrder,
} from "./usages2";

function row(over: Partial<UsageRow2> = {}): UsageRow2 {
  return {
    path: "app/models/order.rb",
    line: 10,
    col: 4,
    kind: "call",
    roles: 0,
    role_names: [],
    trust: "exact",
    precision: "scip-exact",
    context: "total",
    ...over,
  };
}

function out(over: Partial<Usages2Out> = {}): Usages2Out {
  const base: Usages2Out = {
    schema: "usages/2",
    symbol: { name: "total", container: "Order" },
    class_of_definition: "exact",
    exact: [row()],
    likely: [row({ trust: "likely", path: "spec/models/order_spec.rb", roles: ROLE_TEST })],
    candidate: [],
    totals: { exact: 1, likely: 1, candidate: 0, all: 2 },
    capped: [],
    kind_totals: { call: 2 },
    ruby_strict: null,
  };
  return { ...base, ...over };
}

describe("the census reports the SERVER's totals, never a derivation", () => {
  it("uses totals/kind_totals verbatim even when the page is capped", () => {
    const body = out({
      exact: [row(), row({ line: 11 })],
      likely: [],
      totals: { exact: 1841, likely: 0, candidate: 0, all: 1841 },
      kind_totals: { call: 1203, read: 412, write: 9, unclassified: 217 },
      capped: [{ group: "exact", returned: 2, total: 1841, reason: "page" }],
    });
    const c = censusOf(body, NO_CHIPS);
    expect(c.total).toBe(1841);
    // The RETURNED page is two rows; the total is not derived from it.
    expect(c.returned).toBe(2);
    expect(c.byKind.map((k) => k.kind)).toEqual(["call", "read", "unclassified", "write"]);
    expect(c.byKind[0].total).toBe(1203);
  });

  it("emits a reason line for every cap, naming the true total", () => {
    const body = out({
      capped: [{ group: "likely", returned: 500, total: 900, reason: "page" }],
    });
    const c = censusOf(body, NO_CHIPS);
    const r = c.reasons.find((x) => x.id === "capped:likely");
    expect(r).toBeTruthy();
    expect(r!.hiding).toBe(true);
    expect(r!.text).toContain("500 of 900");
  });

  it("a chip that hides rows produces its own on-screen reason", () => {
    const body = out();
    const c = censusOf(body, { ...NO_CHIPS, exclude: ["tests"] });
    expect(c.shown).toBe(1);
    expect(c.returned).toBe(2);
    const r = c.reasons.find((x) => x.id === "chips");
    expect(r?.text).toBe("1 of 2 rows hidden by the chips above");
  });

  it("no chip and no cap ⇒ no hiding reason at all", () => {
    const c = censusOf(out(), NO_CHIPS);
    expect(c.reasons.filter((r) => r.hiding)).toEqual([]);
    expect(c.shown).toBe(c.returned);
  });

  it("the mentions count is its OWN field and is never folded into the total", () => {
    const c = censusOf(out(), NO_CHIPS, 57);
    expect(c.mentions).toBe(57);
    expect(c.total).toBe(2);
    expect(c.reasons.find((r) => r.id === "mentions")?.text).toContain("a different question");
  });

  it("surfaces the Ruby strict verdict when it refused exact", () => {
    const c = censusOf(
      out({ ruby_strict: { exact: false, verdict: "hierarchy-method", hierarchy: ["Order"], sites: 3 } }),
      NO_CHIPS,
    );
    expect(c.reasons.find((r) => r.id === "ruby-strict")?.text).toContain("hierarchy-method");
  });
});

describe("chips", () => {
  it("an empty chip set is the identity", () => {
    expect(chipsAreEmpty(NO_CHIPS)).toBe(true);
    const rows = [row(), row({ trust: "likely" })];
    expect(applyChips(rows, NO_CHIPS)).toEqual(rows);
  });

  it("each exclude chip drops exactly its own role bit", () => {
    const rows = [
      row({ roles: ROLE_TEST }),
      row({ roles: ROLE_VENDOR }),
      row({ roles: 0 }),
    ];
    expect(applyChips(rows, { ...NO_CHIPS, exclude: ["tests"] })).toHaveLength(2);
    expect(applyChips(rows, { ...NO_CHIPS, exclude: ["tests", "vendor"] })).toHaveLength(1);
  });

  it("kind, trust and scope narrow independently", () => {
    const rows = [
      row({ kind: "call", trust: "exact", path: "app/a.rb" }),
      row({ kind: "read", trust: "likely", path: "lib/b.rb" }),
    ];
    expect(applyChips(rows, { ...NO_CHIPS, kinds: ["read"] })).toHaveLength(1);
    expect(applyChips(rows, { ...NO_CHIPS, trust: ["exact"] })).toHaveLength(1);
    expect(applyChips(rows, { ...NO_CHIPS, scope: "app/" })).toHaveLength(1);
  });
});

describe("grouping", () => {
  const rows = [
    row({ path: "app/services/a.rb", kind: "call", trust: "likely", enclosing: { name: "run", kind: "method", line: 3, container: "Svc" } }),
    row({ path: "app/services/b.rb", kind: "call", trust: "exact" }),
    row({ path: "lib/c.rb", kind: "read", trust: "exact" }),
  ];

  it("every declared axis groups without throwing and covers every row", () => {
    for (const axis of GROUP_AXES) {
      const gs = groupRows(rows, axis);
      expect(gs.length, axis).toBeGreaterThan(0);
      expect(walkOrder(gs), axis).toHaveLength(rows.length);
    }
  });

  it("the trust axis renders exact ▷ likely ▷ candidate, never by size", () => {
    const many = [
      ...Array.from({ length: 5 }, () => row({ trust: "likely" })),
      row({ trust: "exact" }),
    ];
    expect(groupRows(many, "trust").map((g) => g.key)).toEqual(["exact", "likely"]);
    expect(TRUST_ORDER[0]).toBe("exact");
  });

  it("other axes are size-descending with a stable tie-break", () => {
    const gs = groupRows(rows, "dir");
    expect(gs.map((g) => g.key)).toEqual(["app/services", "lib"]);
  });

  it("a row with no enclosing symbol still lands in a captioned bucket", () => {
    const gs = groupRows([row({ path: "lib/c.rb", enclosing: undefined })], "enclosing");
    expect(gs[0].key).toBe("lib/c.rb (top level)");
    const ms = groupRows([row({ path: "lib/c.rb", enclosing: undefined })], "module");
    expect(ms[0].key).toBe("lib/ (no module)");
  });
});

describe("the ]u / [u walk", () => {
  it("wraps in both directions and reports -1 for an empty set", () => {
    expect(stepCursor(3, 0, 1)).toBe(1);
    expect(stepCursor(3, 2, 1)).toBe(0);
    expect(stepCursor(3, 0, -1)).toBe(2);
    expect(stepCursor(0, 0, 1)).toBe(-1);
  });

  it("a fresh set steps to the first row forward and the last row back", () => {
    expect(stepCursor(3, -1, 1)).toBe(0);
    expect(stepCursor(3, -1, -1)).toBe(2);
  });
});

it("the title comes from the wire's symbol block", () => {
  expect(usagesTitle(out())).toBe("Order#total");
  expect(usagesTitle(out({ symbol: { name: "total" } }))).toBe("total");
});
