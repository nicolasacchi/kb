// The kbcq/1 LOCK-STEP golden — the SPA half.
//
// `crates/kb-code-server/grammar/kbcq.golden.json` is ONE fixture read by
// TWO parsers: `grammar.rs`'s `golden_corpus_matches_the_rust_parser` and
// this file. Neither parser generates the other, so this is the only thing
// that keeps them from drifting: touch the grammar on one side and the other
// side's golden test fails, naming the case.
//
// Reading across the crate boundary is fine HERE (vitest runs from the repo)
// and is the same thing `src/commands/registry.gen.test.ts` does; only the
// BUNDLE may never import across it (the Docker SPA stage copies `web-code/`
// alone), and `kbcq.ts` itself imports nothing outside `src/`.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  emptyFilters,
  parse,
  FILTER_SPECS,
  LANE_ORDER,
  type Diagnostic,
  type Filters,
  type GroupKey,
} from "./kbcq";

const GOLDEN_PATH = fileURLToPath(
  new URL("../../../crates/kb-code-server/grammar/kbcq.golden.json", import.meta.url),
);

interface GoldenCase {
  raw: string;
  lanes: string[];
  query: string;
  text_mode: string;
  normalized: string;
  filters?: Partial<Filters>;
  sort?: string;
  explain?: boolean;
  group?: GroupKey;
  facets?: boolean;
  diagnostics?: Diagnostic[];
}

const golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8")) as {
  schema: string;
  cases: GoldenCase[];
};

describe("kbcq/1 golden corpus", () => {
  it("declares the kbcq/1 schema and is not empty", () => {
    expect(golden.schema).toBe("kbcq/1");
    expect(golden.cases.length).toBeGreaterThan(0);
  });

  for (const c of golden.cases) {
    it(`parses ${JSON.stringify(c.raw)} exactly as the daemon does`, () => {
      const p = parse(c.raw);
      expect(p.lanes).toEqual(c.lanes);
      expect(p.query).toBe(c.query);
      expect(p.text_mode).toBe(c.text_mode);
      // The fixture omits every default, so compare against a full
      // `Filters` built from the empty one — an omitted key MUST mean "the
      // default", never "unchecked".
      expect(p.filters).toEqual({ ...emptyFilters(), ...(c.filters ?? {}) });
      expect(p.sort).toBe(c.sort ?? null);
      expect(p.explain).toBe(c.explain ?? false);
      expect(p.group).toBe(c.group ?? null);
      expect(p.facets).toBe(c.facets ?? false);
      expect(p.diagnostics).toEqual(c.diagnostics ?? []);
      expect(p.normalized).toBe(c.normalized);
    });
  }

  it("covers every lane and every declared filter key", () => {
    // The same coverage contract the Rust side asserts: this file can only
    // be trusted if the fixture actually exercises the whole grammar.
    for (const lane of LANE_ORDER) {
      expect(
        golden.cases.some((c) => c.lanes.length === 1 && c.lanes[0] === lane),
        `no golden case selects the ${lane} lane alone`,
      ).toBe(true);
    }
    for (const spec of FILTER_SPECS) {
      expect(
        golden.cases.some((c) => c.raw.includes(`${spec.key}:`)),
        `no golden case exercises \`${spec.key}:\``,
      ).toBe(true);
    }
  });

  it("normalize is a fixed point for every golden case", () => {
    for (const c of golden.cases) {
      const once = parse(c.raw);
      const twice = parse(once.normalized);
      expect(twice.normalized, `normalize drifted for ${JSON.stringify(c.raw)}`).toBe(
        once.normalized,
      );
      expect(twice.query).toBe(once.query);
      expect(twice.text_mode).toBe(once.text_mode);
      expect(twice.filters).toEqual(once.filters);
    }
  });
});
