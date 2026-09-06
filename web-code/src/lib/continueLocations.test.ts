import { describe, expect, it } from "vitest";
import { makeLocation, type NavLocation } from "./navHistory";
import { CONTINUE_LOCATIONS_CAP, continueLocations } from "./continueLocations";

function loc(
  path: string,
  line: number,
  opts: Partial<NavLocation> & { repo?: string } = {},
): NavLocation {
  return makeLocation(opts.repo ?? "r", path, line, opts.snippet ?? `line ${line}`, opts.ts ?? line);
}

describe("continueLocations", () => {
  it("is empty for an empty ring", () => {
    expect(continueLocations([])).toEqual([]);
  });

  it("unique by (repo,path), newest first, retains line (unlike recentFilesFrom)", () => {
    const entries = [
      loc("a.rs", 30, { ts: 30 }),
      loc("b.rs", 5, { ts: 20 }),
      loc("a.rs", 1, { ts: 10 }), // duplicate path — older visit, skipped
      loc("c.rs", 8, { repo: "other", ts: 5 }),
    ];
    expect(continueLocations(entries)).toEqual([
      { repo: "r", path: "a.rs", ts: 30, line: 30 },
      { repo: "r", path: "b.rs", ts: 20, line: 5 },
      { repo: "other", path: "c.rs", ts: 5, line: 8 },
    ]);
  });

  it("defaults to a cap of 5", () => {
    expect(CONTINUE_LOCATIONS_CAP).toBe(5);
    const entries = Array.from({ length: 10 }, (_, i) => loc(`f${i}.rs`, i + 1, { ts: 100 - i }));
    expect(continueLocations(entries)).toHaveLength(5);
  });

  it("honors a caller-supplied cap", () => {
    const entries = Array.from({ length: 10 }, (_, i) => loc(`f${i}.rs`, i + 1, { ts: 100 - i }));
    expect(continueLocations(entries, 2)).toHaveLength(2);
  });
});
