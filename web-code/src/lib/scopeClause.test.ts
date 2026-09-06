import { describe, expect, it } from "vitest";
import { parse } from "./kbcq";
import { scopeToClause } from "./scopeClause";

function clauseOf(name: string, globs: string[]): string {
  const r = scopeToClause(name, globs);
  if (!r.ok) throw new Error(`expected a clause, got refusal: ${r.reason}`);
  return r.clause;
}

describe("scopeClause — the degraded scope chip (kbc-scope/1 is not in this tree)", () => {
  it("maps a single-prefix scope to one path: clause the grammar reads back", () => {
    const clause = clauseOf("app", ["app/**"]);
    expect(clause).toBe("path:app/");
    expect(parse(`needle ${clause}`).filters.path).toBe("app/");
  });

  it("maps an all-extension scope to one multi-valued ext: clause", () => {
    const clause = clauseOf("ruby", ["**/*.rb", "*.erb"]);
    expect(clause).toBe("ext:erb|rb");
    expect(parse(`needle ${clause}`).filters.ext).toEqual(["erb", "rb"]);
  });

  it("accepts several globs that all name the SAME directory", () => {
    expect(clauseOf("app", ["app/**", "app"])).toBe("path:app/");
  });

  /// The load-bearing half. Every refusal here is a scope whose meaning
  /// kbcq/1 cannot carry; applying a partial clause would return rows the
  /// scope excludes while LOOKING like it worked.
  it("refuses, with a reason, every scope it cannot say exactly", () => {
    const two = scopeToClause("mixedDirs", ["app/**", "lib/**"]);
    expect(two.ok).toBe(false);
    if (!two.ok) expect(two.reason).toContain("kbc-scope/1");

    const mixed = scopeToClause("mixed", ["app/**", "**/*.rb"]);
    expect(mixed.ok).toBe(false);
    if (!mixed.ok) expect(mixed.reason).toContain("kbc-scope/1");

    // A middle wildcard is neither an extension nor a prefix.
    expect(scopeToClause("tests", ["**/*.test.*"]).ok).toBe(false);
    // A directory-anchored extension glob constrains BOTH axes.
    expect(scopeToClause("appRuby", ["app/*.rb"]).ok).toBe(false);
    expect(scopeToClause("empty", []).ok).toBe(false);
  });

  it("every clause it does emit parses cleanly — no diagnostics, no lost query", () => {
    for (const globs of [["app/**"], ["**/*.rb", "*.erb"], ["config"]]) {
      const r = scopeToClause("s", globs);
      if (!r.ok) continue;
      const p = parse(`needle ${r.clause}`);
      expect(p.diagnostics, `${r.clause} warned`).toEqual([]);
      expect(p.query).toBe("needle");
    }
  });
});
