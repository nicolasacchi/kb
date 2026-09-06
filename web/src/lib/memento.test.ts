// CT-F6 — the SPA half of the lock-step Memento golden. Every case here
// mirrors one in `kb_core::versions::resolve_as_of_tests` (crates/kb-core/
// src/versions.rs): same fixture timestamps (100/200/300), same
// coordinates, same expected pick. If one side changes, this file is where
// the drift surfaces.

import { describe, it, expect } from "vitest";
import { resolveMemento, mementoSummary, parseAtParam } from "./memento";
import type { Version } from "../api/versions";

function v(ref: string, source: Version["source"], ts_unix: number): Version {
  return { ref, source, label: "", author: "", ts_unix, short: ref };
}

// The façade's own newest-first order (working tree, then git/index by
// ts_unix descending) — the precondition both resolvers document.
const TIMELINE: Version[] = [
  v("WORKING", "working", 300),
  v("deadbeef", "git", 200),
  v("index:1", "index", 100),
];

describe("resolveMemento", () => {
  it("picks the nearest PRIOR version and never rounds forward", () => {
    const m = resolveMemento(TIMELINE, 250);
    expect(m.version?.ref).toBe("deadbeef");
    expect(m.version?.ts_unix).toBe(200);
    expect(m.exact).toBe(false);
    expect(m.atUnix).toBe(250);
    expect(m.oldestTsUnix).toBe(100);
  });

  it("flags an exact hit when the instant IS the version's timestamp", () => {
    const m = resolveMemento(TIMELINE, 200);
    expect(m.version?.ref).toBe("deadbeef");
    expect(m.exact).toBe(true);
  });

  // Mirrors `golden_working_wins_when_at_or_after_its_own_ts`.
  it("resolves an instant at or after the working tree to the working tree", () => {
    expect(resolveMemento(TIMELINE, 300).version?.ref).toBe("WORKING");
    expect(resolveMemento(TIMELINE, 500).version?.ref).toBe("WORKING");
  });

  // Mirrors `golden_tie_at_exact_boundary_picks_the_first_in_order`.
  it("breaks a same-second tie by the given order, not by ref", () => {
    const tied = [v("a", "git", 200), v("b", "git", 200)];
    expect(resolveMemento(tied, 200).version?.ref).toBe("a");
  });

  // Mirrors `golden_below_the_oldest_version_is_none` — the load-bearing
  // one: too-old is a MISS, never a silent fallback to the oldest.
  it("returns an explicit miss (never the oldest) when nothing is that old", () => {
    const m = resolveMemento(TIMELINE, 50);
    expect(m.version).toBeNull();
    expect(m.exact).toBe(false);
    expect(m.oldestTsUnix).toBe(100);
  });

  it("is a miss with no floor for an empty timeline", () => {
    const m = resolveMemento([], 1000);
    expect(m.version).toBeNull();
    expect(m.oldestTsUnix).toBeNull();
  });

  // Mirrors `golden_non_monotonic_author_dates_trusts_the_given_order_only`.
  it("trusts the given order only (post-rebase author dates)", () => {
    const rebased = [v("c2", "git", 500), v("c1", "git", 100)];
    expect(resolveMemento(rebased, 150).version?.ref).toBe("c1");
    expect(resolveMemento(rebased, 500).version?.ref).toBe("c2");
  });
});

describe("mementoSummary", () => {
  const fmt = (ts: number) => `T${ts}`;

  it("always names the relation on a nearest-prior hit", () => {
    const s = mementoSummary(resolveMemento(TIMELINE, 250), fmt);
    expect(s).toBe("deadbeef @ T200 (nearest prior)");
  });

  it("says exact match when the instant landed on the version", () => {
    const s = mementoSummary(resolveMemento(TIMELINE, 200), fmt);
    expect(s).toContain("(exact match)");
    expect(s).not.toContain("nearest prior");
  });

  it("names the floor on a miss and points at no version", () => {
    const s = mementoSummary(resolveMemento(TIMELINE, 50), fmt);
    expect(s).toContain("that old");
    expect(s).toContain("T100");
    expect(s).not.toContain("index:1");
  });

  it("degrades honestly for an empty timeline", () => {
    expect(mementoSummary(resolveMemento([], 50), fmt)).toContain(
      "no recorded versions",
    );
  });
});

describe("parseAtParam", () => {
  it("parses unix seconds", () => {
    expect(parseAtParam("1700000000")).toBe(1700000000);
    expect(parseAtParam(" 1700000000 ")).toBe(1700000000);
    expect(parseAtParam("0")).toBe(0);
    expect(parseAtParam("-5")).toBe(-5);
  });

  it("is TOTAL — malformed values degrade to null, never a fabricated instant", () => {
    expect(parseAtParam(null)).toBeNull();
    expect(parseAtParam("")).toBeNull();
    expect(parseAtParam("2026-07-30")).toBeNull();
    expect(parseAtParam("1.5")).toBeNull();
    expect(parseAtParam("NaN")).toBeNull();
    expect(parseAtParam("1e9")).toBeNull();
    expect(parseAtParam("99999999999999999999")).toBeNull();
  });
});
