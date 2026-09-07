// The kbc-hunkid/1 LOCK-STEP golden — the SPA half.
//
// `crates/kb-code-server/grammar/kbchunkid.golden.json` is ONE fixture read by
// TWO implementations of one address: `crates/kb-code-server/src/
// review_hunks.rs`'s `golden_corpus_matches_the_rust_implementation` and this
// file's walk over the same bytes through the REAL SPA functions
// (`parseUnifiedDiff` + `hunkFingerprintInput` + `hunkId`). Neither generates
// the other, so the fixture is the only thing keeping them from drifting:
// change the recipe on one side and the other side fails, naming the case.
//
// V73-K2a minted this address client-side because the daemon stored it
// opaquely (`review_hunk_viewed`, V0031). V73-K3's hunk↔turn join asks the
// daemon to FIND the hunk an id names, so the daemon needed the recipe too —
// hence a second implementation, hence this golden.
//
// The load-bearing detail the golden exists to protect: the hash is FNV-1a 64
// over **UTF-16 code units** (`charCodeAt`), not bytes. The `non-BMP and
// accented content` case would be the only one to disagree if either side
// "simplified" to bytes.
//
// Reading across the crate boundary is fine HERE (vitest runs from the repo)
// — the same thing `kbcq.golden.test.ts`, `kbcRefs.golden.test.ts` and
// `src/commands/registry.gen.test.ts` already do; only the BUNDLE may never
// import across it. The fixture lives on the CRATE side because the Rust
// builder stage's Docker context is `COPY crates ./crates`.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { parseUnifiedDiff } from "./diff";
import { hunkFingerprintInput, hunkId, HUNK_ID_SCHEMA } from "./diffHunks";

const GOLDEN_PATH = fileURLToPath(
  new URL(
    "../../../crates/kb-code-server/grammar/kbchunkid.golden.json",
    import.meta.url,
  ),
);

interface GoldenHunk {
  fingerprint_input: string;
  id: string;
  removed_text: string;
  added_text: string;
}

interface GoldenCase {
  name: string;
  path: string;
  diff: string;
  hunks: GoldenHunk[];
}

interface Golden {
  schema: string;
  note: string;
  cases: GoldenCase[];
}

const golden: Golden = JSON.parse(readFileSync(GOLDEN_PATH, "utf8"));

function sideText(
  hunk: ReturnType<typeof parseUnifiedDiff>["hunks"][number],
  kind: "add" | "remove",
): string {
  return hunk.lines
    .filter((l) => l.kind === kind)
    .map((l) => l.text)
    .join("\n");
}

describe("kbc-hunkid/1 golden", () => {
  it("declares the schema this module implements", () => {
    expect(golden.schema).toBe(HUNK_ID_SCHEMA);
    expect(golden.cases.length).toBeGreaterThan(0);
  });

  it.each(golden.cases.map((c) => [c.name, c] as const))(
    "%s — parses, fingerprints and hashes exactly as the fixture says",
    (_name, c) => {
      const parsed = parseUnifiedDiff(c.diff);
      expect(parsed.hunks).toHaveLength(c.hunks.length);
      parsed.hunks.forEach((h, i) => {
        const want = c.hunks[i];
        expect(hunkFingerprintInput(c.path, h)).toBe(want.fingerprint_input);
        expect(hunkId(c.path, h)).toBe(want.id);
        expect(sideText(h, "remove")).toBe(want.removed_text);
        expect(sideText(h, "add")).toBe(want.added_text);
        expect(want.id).toMatch(/^[0-9a-f]{16}$/);
      });
    },
  );

  it("keeps a non-BMP case, which is the only one a byte-based hash breaks", () => {
    const names = golden.cases.map((c) => c.name);
    expect(names).toContain("non-BMP and accented content");
  });
});
