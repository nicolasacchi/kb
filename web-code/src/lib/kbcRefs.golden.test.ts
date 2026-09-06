// The kbc-refs/1 LOCK-STEP golden — the SPA half, seeded ahead of the parser.
//
// `crates/kb-code-server/grammar/kbcrefs.golden.json` is ONE fixture meant to
// be read by TWO parsers: `review_doc/refs.rs`'s
// `golden_corpus_matches_the_rust_parser` and — once K2 lands
// `web-code/src/lib/kbcRefs.ts` — this file's own walk over the same bytes.
// Neither parser generates the other, so the fixture is the only thing that
// keeps them from drifting: touch the grammar on one side and the other
// side's golden test fails, naming the case.
//
// K1 ships the SERVER parser only, so this file deliberately does NOT import
// a TS parser yet. What it does today is assert the fixture is present,
// well-formed, and covers every declared scheme — so the file cannot rot
// between now and K2, and so K2's first job is a one-line import plus the
// commented-out block at the bottom rather than "invent a fixture".
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

  // K2 (the SPA parser) replaces this file's body with the real walk:
  //
  //   import { classify } from "./kbcRefs";
  //   for (const c of golden.cases) {
  //     it(`classifies ${JSON.stringify(c.body)} exactly as the daemon does`, () => {
  //       const got = classify(c.body);
  //       expect(got.kind).toBe(c.class);
  //       if (c.class === "ref") expect(got.ref).toEqual(c.parsed);
  //     });
  //   }
  //
  // Until then the assertions above keep the fixture honest and present.
});
