// The kbc-refs/1 LOCK-STEP golden — the SPA half (V73-K2b).
//
// `crates/kb-code-server/grammar/kbcrefs.golden.json` is ONE fixture read by
// TWO parsers: `review_doc/refs.rs`'s `golden_corpus_matches_the_rust_parser`
// and this file's walk over the same bytes through `lib/kbcRefs.ts`. Neither
// parser generates the other, so the fixture is the only thing that keeps
// them from drifting: touch the grammar on one side and the other side's
// test fails, naming the case.
//
// K1 shipped the SERVER parser and left this file asserting only that the
// fixture was present and well-formed. K2b lands the SPA parser, so the walk
// K1 left commented out at the bottom is now the point of the file — the
// fixture-integrity assertions stay, because a fixture that stops covering a
// scheme would make the walk pass for the wrong reason.
//
// Reading across the crate boundary is fine HERE (vitest runs from the repo)
// and is the same thing `kbcq.golden.test.ts` and `src/commands/
// registry.gen.test.ts` already do; only the BUNDLE may never import across
// it (the Docker SPA stage copies `web-code/` alone). The fixture lives on
// the CRATE side for the mirror-image reason: the Rust builder stage's
// context is `COPY crates ./crates`, so an `include_str!` pointing into
// `web-code/` would not build.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { classify, SCHEMES as TS_SCHEMES } from "./kbcRefs";

const GOLDEN_PATH = fileURLToPath(
  new URL(
    "../../../crates/kb-code-server/grammar/kbcrefs.golden.json",
    import.meta.url,
  ),
);

/** The seven scheme prefixes `review_doc::refs::SCHEMES` declares. */
const SCHEMES = ["code", "sym", "ent", "finding", "gh", "kb", "hunk"] as const;

type RefClass = "ref" | "wikilink" | "malformed";

interface GoldenCase {
  body: string;
  class: RefClass;
  parsed?: Record<string, unknown>;
}

const golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8")) as {
  schema: string;
  note: string;
  cases: GoldenCase[];
};

describe("kbc-refs/1 golden corpus", () => {
  it("declares the kbc-refs/1 schema and is not empty", () => {
    expect(golden.schema).toBe("kbc-refs/1");
    expect(golden.cases.length).toBeGreaterThan(0);
  });

  it("uses only the three declared classes", () => {
    for (const c of golden.cases) {
      expect(["ref", "wikilink", "malformed"]).toContain(c.class);
    }
  });

  it("carries a parsed shape for every ref case and none for the others", () => {
    for (const c of golden.cases) {
      if (c.class === "ref") {
        expect(c.parsed, `${c.body} is a ref and needs a parsed shape`).toBeTruthy();
        expect(c.parsed?.scheme).toBe(c.body.slice(0, c.body.indexOf(":")));
        expect(c.parsed?.raw).toBe(c.body);
      } else {
        expect(c.parsed, `${c.body} is ${c.class} and must carry no parsed shape`)
          .toBeUndefined();
      }
    }
  });

  it("covers every declared scheme with at least one well-formed case", () => {
    for (const scheme of SCHEMES) {
      const hit = golden.cases.some(
        (c) => c.class === "ref" && c.body.startsWith(`${scheme}:`),
      );
      expect(hit, `no well-formed ${scheme}: case in the golden corpus`).toBe(true);
    }
  });

  it("pins that a bare wikilink is never a kbc ref (root invariant #29)", () => {
    const bare = golden.cases.filter((c) => c.class === "wikilink");
    expect(bare.length).toBeGreaterThan(0);
    for (const c of bare) {
      const scheme = c.body.slice(0, Math.max(0, c.body.indexOf(":")));
      expect(
        SCHEMES as readonly string[],
        `${c.body} is classed as a wikilink but names a kbc scheme`,
      ).not.toContain(scheme);
    }
  });

  it("declares the same seven schemes the daemon does", () => {
    expect([...TS_SCHEMES]).toEqual([...SCHEMES]);
  });
});

// The walk itself: every case, both sides, one assertion each so a failure
// names the body that diverged rather than "the corpus".
describe("kbc-refs/1 — the TS parser classifies exactly as the daemon does", () => {
  for (const c of golden.cases) {
    it(`classifies ${JSON.stringify(c.body)} as ${c.class}`, () => {
      const got = classify(c.body);
      expect(got.kind).toBe(c.class);
      if (got.kind === "ref") {
        // `parsed` IS the serde serialization of the Rust `Ref` — an extra
        // or missing key here is a real divergence, not a shape preference.
        expect(got.ref).toEqual(c.parsed);
        expect(Object.keys(got.ref).sort()).toEqual(Object.keys(c.parsed ?? {}).sort());
      }
      if (got.kind === "malformed") {
        // A malformed ref must always say WHY — an unexplained refusal is
        // the invisible failure invariant 22(b) exists to prevent.
        expect(got.reason.length).toBeGreaterThan(0);
      }
    });
  }
});
