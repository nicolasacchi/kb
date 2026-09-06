import { describe, expect, it } from "vitest";
import type { DefsOut, RefsOut, ResolveCandidate } from "../api/types";
import {
  currentRow,
  defRowsFrom,
  initialPeekState,
  peekReducer,
  refRowsFrom,
  resolveCandidateToRow,
  singleExactMatch,
  type HoverCard,
  type PeekRow,
  type PeekState,
} from "./peekState";

const ROWS: PeekRow[] = [
  { repo: "r", path: "a.rs", line: 1, symbolKind: "function", container: null, approximate: false },
  { repo: "r", path: "b.rs", line: 2, symbolKind: "function", container: null, approximate: false },
  { repo: "r", path: "c.rs", line: 3, symbolKind: "function", container: null, approximate: false },
];

function stateWith(patch: Partial<PeekState>): PeekState {
  return { ...initialPeekState, open: true, rows: ROWS, ...patch };
}

describe("initialPeekState", () => {
  it("starts closed, empty, cursor at 0", () => {
    expect(initialPeekState).toEqual({
      open: false,
      mode: "defs",
      word: "",
      loading: false,
      error: null,
      rows: [],
      cursor: 0,
      approximate: false,
    });
  });
});

describe("OPEN", () => {
  it("opens empty + loading, resetting any stale prior state", () => {
    const prior = stateWith({ cursor: 2, error: "boom", approximate: true, note: "stale" });
    const next = peekReducer(prior, { type: "OPEN", mode: "refs", word: "foo" });
    expect(next).toEqual({
      open: true,
      mode: "refs",
      word: "foo",
      loading: true,
      error: null,
      rows: [],
      cursor: 0,
      approximate: false,
    });
  });
});

describe("OPEN_WITH_ROWS", () => {
  it("opens directly populated, not loading", () => {
    const next = peekReducer(initialPeekState, {
      type: "OPEN_WITH_ROWS",
      mode: "defs",
      word: "run",
      rows: ROWS,
      approximate: true,
    });
    expect(next.open).toBe(true);
    expect(next.loading).toBe(false);
    expect(next.rows).toBe(ROWS);
    expect(next.approximate).toBe(true);
    expect(next.cursor).toBe(0);
  });
});

describe("SET_ROWS", () => {
  it("fills rows, clears loading, clamps an out-of-range cursor", () => {
    const prior = stateWith({ rows: [], loading: true, cursor: 5 });
    const next = peekReducer(prior, {
      type: "SET_ROWS",
      rows: ROWS,
      approximate: false,
      note: "a note",
      counts: { defs: 3, refs: 0 },
    });
    expect(next.loading).toBe(false);
    expect(next.rows).toBe(ROWS);
    expect(next.note).toBe("a note");
    expect(next.counts).toEqual({ defs: 3, refs: 0 });
    expect(next.cursor).toBe(2); // clamped to ROWS.length - 1
  });

  it("preserves an in-range cursor across a refresh", () => {
    const prior = stateWith({ cursor: 1 });
    const next = peekReducer(prior, { type: "SET_ROWS", rows: ROWS, approximate: false });
    expect(next.cursor).toBe(1);
  });

  it("resolves an empty result set to cursor 0", () => {
    const prior = stateWith({ cursor: 2 });
    const next = peekReducer(prior, { type: "SET_ROWS", rows: [], approximate: false });
    expect(next.cursor).toBe(0);
    expect(next.rows).toEqual([]);
  });
});

describe("SET_ERROR", () => {
  it("clears rows/loading and records the message", () => {
    const prior = stateWith({ loading: true, counts: { defs: 1, refs: 1 } });
    const next = peekReducer(prior, { type: "SET_ERROR", message: "network down" });
    expect(next.loading).toBe(false);
    expect(next.error).toBe("network down");
    expect(next.rows).toEqual([]);
    expect(next.counts).toBeUndefined();
  });
});

describe("MOVE", () => {
  it("moves the cursor down within the row list", () => {
    const next = peekReducer(stateWith({ cursor: 0 }), { type: "MOVE", delta: 1 });
    expect(next.cursor).toBe(1);
  });

  it("clamps at the last row", () => {
    const next = peekReducer(stateWith({ cursor: 2 }), { type: "MOVE", delta: 1 });
    expect(next.cursor).toBe(2);
  });

  it("clamps at the first row", () => {
    const next = peekReducer(stateWith({ cursor: 0 }), { type: "MOVE", delta: -1 });
    expect(next.cursor).toBe(0);
  });

  it("is a no-op with no rows", () => {
    const empty = stateWith({ rows: [] });
    expect(peekReducer(empty, { type: "MOVE", delta: 1 })).toBe(empty);
  });
});

describe("CLOSE", () => {
  it("resets fully to initial state", () => {
    const next = peekReducer(stateWith({ cursor: 2, word: "foo" }), { type: "CLOSE" });
    expect(next).toEqual(initialPeekState);
  });

  it("is a no-op when already closed", () => {
    expect(peekReducer(initialPeekState, { type: "CLOSE" })).toBe(initialPeekState);
  });
});

describe("currentRow", () => {
  it("returns the row at the cursor", () => {
    expect(currentRow(stateWith({ cursor: 1 }))?.path).toBe("b.rs");
  });

  it("returns null for an empty row list", () => {
    expect(currentRow(stateWith({ rows: [] }))).toBeNull();
  });
});

// --- server-response mapping -------------------------------------------------

function defsOut(overrides: Partial<DefsOut> = {}): DefsOut {
  return {
    schema: "defs/1",
    symbol: "run",
    exact: true,
    results: [
      {
        repo: "r",
        path: "a.rs",
        ordinal: 0,
        name: "run",
        kind: "function",
        line_start: 10,
        line_end: 12,
        col_start: 0,
        col_end: 3,
        container: "Runner",
        signature: null,
        doc: null,
        approximate: false,
      },
    ],
    ...overrides,
  };
}

function refsOut(overrides: Partial<RefsOut> = {}): RefsOut {
  return {
    schema: "refs/1",
    repo: "r",
    symbol: "run",
    results: [{ path: "a.rs", line: 10, text: "fn run() {}", approximate: true }],
    truncated: false,
    time_budget_exceeded: false,
    note: "text-grep, tags-tier match",
    ...overrides,
  };
}

describe("defRowsFrom", () => {
  it("maps a DefHit to a PeekRow, using line_start as the target line", () => {
    const rows = defRowsFrom(defsOut());
    expect(rows).toEqual([
      { repo: "r", path: "a.rs", line: 10, symbolKind: "function", container: "Runner", approximate: false },
    ]);
  });
});

describe("refRowsFrom", () => {
  it("maps a RefHit to a PeekRow, carrying the repo from the response", () => {
    const rows = refRowsFrom(refsOut());
    expect(rows).toEqual([{ repo: "r", path: "a.rs", line: 10, text: "fn run() {}", approximate: true }]);
  });
});

// --- B3: resolve candidate → row, hover card actions ------------------------

function resolveCandidate(overrides: Partial<ResolveCandidate> = {}): ResolveCandidate {
  return {
    repo: "r",
    path: "a.rs",
    line: 1,
    kind: "fn",
    container: null,
    signature: "fn widget() -> i32",
    doc: "widget doc",
    precision: "file-local",
    ...overrides,
  };
}

describe("resolveCandidateToRow", () => {
  it("maps a resolve candidate to a row, carrying precision along and approximate=false", () => {
    const row = resolveCandidateToRow(resolveCandidate({ container: "Runner" }));
    expect(row).toEqual({
      repo: "r",
      path: "a.rs",
      line: 1,
      symbolKind: "fn",
      container: "Runner",
      approximate: false,
      precision: "file-local",
    });
  });

  it("maps a null kind to an undefined symbolKind", () => {
    const row = resolveCandidateToRow(resolveCandidate({ kind: null }));
    expect(row.symbolKind).toBeUndefined();
  });

  it("carries the candidate's trust class along as trustClass (T1)", () => {
    const row = resolveCandidateToRow(resolveCandidate({ class: "exact" }));
    expect(row.trustClass).toBe("exact");
  });

  it("trustClass is undefined for an older-daemon candidate with no class field", () => {
    const { class: _omit, ...noClass } = resolveCandidate();
    const row = resolveCandidateToRow(noClass as ResolveCandidate);
    expect(row.trustClass).toBeUndefined();
  });
});

describe("SET_CARD", () => {
  it("opens the card, clears loading/error, and records the response note", () => {
    const prior = { ...initialPeekState, open: true, mode: "hover" as const, loading: true };
    const card: HoverCard = { ident: "widget", role: "ref", candidate: resolveCandidate() };
    const next = peekReducer(prior, { type: "SET_CARD", card, note: "resolve note" });
    expect(next.loading).toBe(false);
    expect(next.error).toBeNull();
    expect(next.card).toBe(card);
    expect(next.note).toBe("resolve note");
  });
});

describe("SET_CARD_PROVENANCE", () => {
  it("merges the provenance line onto the existing card", () => {
    const card: HoverCard = { ident: "widget", role: "ref", candidate: resolveCandidate() };
    const prior = { ...initialPeekState, open: true, mode: "hover" as const, card };
    const provenance = { none: false, displayName: "a session", commitSubject: "did a thing" };
    const next = peekReducer(prior, { type: "SET_CARD_PROVENANCE", provenance });
    expect(next.card?.provenance).toEqual(provenance);
    // The candidate itself is untouched.
    expect(next.card?.candidate).toBe(card.candidate);
  });

  it("is a no-op when there is no card", () => {
    const prior = { ...initialPeekState, open: true, mode: "hover" as const };
    const provenance = { none: true, displayName: null, commitSubject: null };
    const next = peekReducer(prior, { type: "SET_CARD_PROVENANCE", provenance });
    expect(next).toBe(prior);
  });
});

describe("singleExactMatch", () => {
  it("returns the hit when exact and exactly one result", () => {
    const hit = singleExactMatch(defsOut());
    expect(hit?.path).toBe("a.rs");
  });

  it("returns null when exact but ambiguous (more than one)", () => {
    const out = defsOut({ results: [...defsOut().results, { ...defsOut().results[0], path: "b.rs" }] });
    expect(singleExactMatch(out)).toBeNull();
  });

  it("returns null when not exact (fuzzy fallback), regardless of count", () => {
    const out = defsOut({ exact: false });
    expect(singleExactMatch(out)).toBeNull();
  });

  it("returns null when there are no results at all", () => {
    const out = defsOut({ exact: false, results: [] });
    expect(singleExactMatch(out)).toBeNull();
  });
});
