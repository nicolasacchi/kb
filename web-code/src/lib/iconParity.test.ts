// SH.I1 — cross-SPA icon parity golden.
//
// web/ and web-code/ each own a copy of the Icon set (separate build-isolated
// apps — no shared module, by design; see web-code/src/components/icons.tsx's
// header). Their stated contract is "same design family": any glyph NAME the
// two files share must render the same shapes. This test pins that contract
// the only way two un-importable files can be pinned — by reading both
// sources off disk and comparing each shared entry's SVG body, normalized
// for the one legitimate divergence (the default render size passed to
// `base(N)`; a 14px web glyph may render 12px in web-code's denser chrome).
//
// If this fails you edited a shared glyph in one file only: apply the same
// path/attribute change to the sibling SPA's icons.tsx (or rename the glyph
// if the divergence is intentional — a new name is a new contract).
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const OWN = fileURLToPath(new URL("../components/icons.tsx", import.meta.url));
const SIBLING = fileURLToPath(
  new URL("../../../web/src/components/icons.tsx", import.meta.url),
);

/** Extract `Name -> normalized svg body` for every entry in an icons.tsx. */
function parseIcons(path: string): Map<string, string> {
  const src = readFileSync(path, "utf8");
  const out = new Map<string, string>();
  // Entries are uniformly `  Name: (p: P) => (\n ... \n  ),` — both files are
  // hand-kept in this exact shape (same author, same grammar).
  const entry = /^ {2}(\w+): \(p: P\) => \(\n([\s\S]*?)\n {2}\),$/gm;
  for (const m of src.matchAll(entry)) {
    const name = m[1];
    const body = m[2]
      // The default render size is per-SPA context, not part of the glyph.
      .replace(/\{\.\.\.base\(\d+\)\}/g, "{...base(N)}")
      .replace(/\s+/g, " ")
      .trim();
    out.set(name, body);
  }
  return out;
}

describe("icon parity across web/ and web-code/", () => {
  const own = parseIcons(OWN);
  const sibling = parseIcons(SIBLING);

  it("parses a plausible number of entries from both files", () => {
    // Regex-extraction sanity: if a reformat breaks parsing, fail loudly
    // instead of silently comparing empty sets.
    expect(own.size).toBeGreaterThan(30);
    expect(sibling.size).toBeGreaterThan(30);
  });

  it("shares a substantial family of glyph names", () => {
    const shared = [...own.keys()].filter((k) => sibling.has(k));
    expect(shared.length).toBeGreaterThanOrEqual(25);
  });

  it("renders identical shapes for every shared glyph name", () => {
    const drifted: string[] = [];
    for (const [name, body] of own) {
      const other = sibling.get(name);
      if (other !== undefined && other !== body) drifted.push(name);
    }
    // Print the offending names in the failure message, not just a count.
    expect(drifted, `drifted glyphs: ${drifted.join(", ")}`).toEqual([]);
  });
});
