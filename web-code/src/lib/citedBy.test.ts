import { describe, expect, it } from "vitest";
import type { DocRefClaim, LiveLookup } from "../api/types";
import { citedByDocCount, citedByLabel, citedByNote, citedByRowLabel, siblingDocChip } from "./citedBy";

function claim(overrides: Partial<DocRefClaim>): DocRefClaim {
  return {
    kb: "platform",
    doc_id: "doc1",
    doc_title: "Fixture Doc",
    doc_path: "docs/fixture-doc.html",
    doc_public_href: "https://kb.example/a/platform/docs/fixture-doc.html",
    group_anchor: null,
    doc_section_href: null,
    group_label: null,
    raw_hint: "resolver.rs:2",
    line_start: 2,
    line_end: 2,
    line_state: "confirmed",
    seen_at: 1_754_500_000,
    source: "pinned",
    ...overrides,
  };
}

describe("citedByDocCount", () => {
  it("empty claims ⇒ 0", () => {
    expect(citedByDocCount([])).toBe(0);
  });

  it("one claim ⇒ 1", () => {
    expect(citedByDocCount([claim({})])).toBe(1);
  });

  it("two claims from the SAME doc (two ordinals citing the same path) ⇒ 1, not 2", () => {
    expect(
      citedByDocCount([
        claim({ doc_id: "doc1", raw_hint: "resolver.rs:2" }),
        claim({ doc_id: "doc1", raw_hint: "resolver.rs:9999" }),
      ]),
    ).toBe(1);
  });

  it("claims from two distinct docs ⇒ 2", () => {
    expect(citedByDocCount([claim({ doc_id: "doc1" }), claim({ doc_id: "doc4" })])).toBe(2);
  });

  it("three claims, two distinct docs ⇒ 2 (dedup, not a raw count)", () => {
    expect(
      citedByDocCount([
        claim({ doc_id: "doc1", raw_hint: "resolver.rs:2" }),
        claim({ doc_id: "doc1", raw_hint: "resolver.rs:9999" }),
        claim({ doc_id: "doc4" }),
      ]),
    ).toBe(2);
  });

  it("same doc_id, DIFFERENT kb ⇒ 2 — doc identity on this wire is (kb, doc_id), not doc_id alone (m3)", () => {
    expect(
      citedByDocCount([
        claim({ kb: "platform", doc_id: "doc1" }),
        claim({ kb: "other-kb", doc_id: "doc1" }),
      ]),
    ).toBe(2);
  });
});

describe("citedByLabel", () => {
  it("singular doc, live ⇒ 'Cited by 1 doc'", () => {
    expect(citedByLabel([claim({})], true)).toBe("Cited by 1 doc");
  });

  it("plural docs, live ⇒ 'Cited by N docs'", () => {
    expect(citedByLabel([claim({ doc_id: "doc1" }), claim({ doc_id: "doc4" })], true)).toBe("Cited by 2 docs");
  });

  it("zero claims, live ⇒ 'Cited by 0 docs' (the caller is responsible for not rendering this case)", () => {
    expect(citedByLabel([], true)).toBe("Cited by 0 docs");
  });

  it("not live ⇒ the whole strip carries the rot note, once, regardless of claim count", () => {
    expect(citedByLabel([claim({})], false)).toBe("Cited by 1 doc — path no longer present");
    expect(citedByLabel([claim({ doc_id: "doc1" }), claim({ doc_id: "doc4" })], false)).toBe(
      "Cited by 2 docs — path no longer present",
    );
  });

  it("dedups by doc_id the same way citedByDocCount does, and — since claims (2) ≠ docs (1) — appends the raw citation count (m9)", () => {
    const claims = [
      claim({ doc_id: "doc1", raw_hint: "resolver.rs:2" }),
      claim({ doc_id: "doc1", raw_hint: "resolver.rs:9999" }),
    ];
    expect(citedByLabel(claims, true)).toBe("Cited by 1 doc · 2 citations");
  });

  it("m9: claim count equals doc count ⇒ no '· M citations' suffix (already covered implicitly above, asserted explicitly here)", () => {
    expect(citedByLabel([claim({ doc_id: "doc1" }), claim({ doc_id: "doc4" })], true)).toBe("Cited by 2 docs");
  });

  it("m9: claim count exceeds doc count AND not live ⇒ both suffixes, citation count before the rot note", () => {
    const claims = [
      claim({ doc_id: "doc1", raw_hint: "resolver.rs:2" }),
      claim({ doc_id: "doc1", raw_hint: "resolver.rs:9999" }),
      claim({ doc_id: "doc4" }),
    ];
    expect(citedByLabel(claims, false)).toBe("Cited by 2 docs · 3 citations — path no longer present");
  });
});

// Mid-flight W3.A review note, corrected by W3.B.R (M2): the server NEVER
// persists an empty `doc_title` — `sync.rs`'s `doc_title_or_fallback`
// (DCB-W3.A.R fix 6) already falls back to the doc path's basename, then
// the doc id, at WRITE time. This suite exercises the CLIENT's own copy of
// that ladder as redundant-but-safe defense-in-depth, not a real-world gap:
// the e2e `doc5` fixture (`doc-lens.spec.ts`) is what exercises the
// SERVER-side fallback end to end.
describe("citedByRowLabel", () => {
  it("a real title ⇒ the title, verbatim", () => {
    expect(citedByRowLabel(claim({ doc_title: "Fixture Doc" }))).toBe("Fixture Doc");
  });

  it("empty title ⇒ falls back to the doc_path basename", () => {
    expect(citedByRowLabel(claim({ doc_title: "", doc_path: "docs/citedby-b.html" }))).toBe("citedby-b.html");
  });

  it("whitespace-only title ⇒ treated as empty, falls back to the basename", () => {
    expect(citedByRowLabel(claim({ doc_title: "   ", doc_path: "docs/citedby-b.html" }))).toBe("citedby-b.html");
  });

  it("empty title AND empty doc_path ⇒ falls back to doc_id", () => {
    expect(citedByRowLabel(claim({ doc_title: "", doc_path: "", doc_id: "doc5" }))).toBe("doc5");
  });

  it("empty title, doc_path with only slashes ⇒ falls back to doc_id", () => {
    expect(citedByRowLabel(claim({ doc_title: "", doc_path: "///", doc_id: "doc5" }))).toBe("doc5");
  });

  it("strips directories, keeping only the basename", () => {
    expect(citedByRowLabel(claim({ doc_title: "", doc_path: "a/b/c/leaf.html" }))).toBe("leaf.html");
  });
});

describe("siblingDocChip", () => {
  const SEC = "https://kb.example/a/platform/docs/fixture-doc.html?sec=kb-h-a";
  it("zero claims => null", () => {
    expect(siblingDocChip([], true, null)).toBeNull();
  });

  it("two claims in the same doc => one link, preferring the section href", () => {
    const m = siblingDocChip(
      [claim({ doc_section_href: SEC }), claim({ raw_hint: "resolver.rs:9", line_start: 9, line_end: 9 })],
      true,
      null,
    );
    expect(m?.kind).toBe("link");
    expect(m?.href).toBe(SEC);
    expect(m?.count).toBe(1);
    expect(m?.tone).toBe("ok");
  });

  it("within one doc the claim covering the line decides the section", () => {
    const grouped = claim({ line_start: 2, line_end: 2, doc_section_href: SEC });
    const ungrouped = claim({
      raw_hint: "y.rs:90",
      line_start: 90,
      line_end: 90,
      group_anchor: null,
      doc_section_href: null,
      doc_public_href: "https://kb.example/a/platform/docs/fixture-doc.html",
    });
    expect(siblingDocChip([grouped, ungrouped], true, 90)?.href).toBe(
      "https://kb.example/a/platform/docs/fixture-doc.html",
    );
    expect(siblingDocChip([grouped, ungrouped], true, 2)?.href).toBe(SEC);
  });

  it("two distinct docs with no overlap => open-strip", () => {
    const m = siblingDocChip([claim({ doc_id: "doc1" }), claim({ doc_id: "doc4" })], true, null);
    expect(m?.kind).toBe("open-strip");
    expect(m?.href).toBeNull();
    expect(m?.count).toBe(2);
  });

  it("a line overlap picks the right doc", () => {
    const a = claim({ doc_id: "doc1", line_start: 2, line_end: 2, doc_public_href: "https://kb.example/a/p/one.html" });
    const b = claim({ doc_id: "doc4", line_start: 40, line_end: 50, doc_public_href: "https://kb.example/a/p/four.html" });
    const m = siblingDocChip([a, b], true, 45);
    expect(m?.kind).toBe("link");
    expect(m?.href).toBe("https://kb.example/a/p/four.html");
    // Both docs overlap => still ambiguous.
    const c = claim({ doc_id: "doc5", line_start: 44, line_end: 46 });
    expect(siblingDocChip([a, b, c], true, 45)?.kind).toBe("open-strip");
  });

  it("a null section href falls back to the doc-level href", () => {
    const m = siblingDocChip([claim({ doc_section_href: null })], true, null);
    expect(m).toMatchObject({ kind: "link", href: "https://kb.example/a/platform/docs/fixture-doc.html" });
  });

  it("a null public base (no hrefs) => open-strip", () => {
    const m = siblingDocChip([claim({ doc_public_href: null, doc_section_href: null })], true, null);
    expect(m?.kind).toBe("open-strip");
  });

  it("an unsafe href is never linked", () => {
    const m = siblingDocChip([claim({ doc_section_href: "javascript:alert(1)", doc_public_href: null })], true, null);
    expect(m?.kind).toBe("open-strip");
  });

  it("live=false => warn tone and the label suffix from citedByLabel", () => {
    const claims = [claim({})];
    const m = siblingDocChip(claims, false, null);
    expect(m?.tone).toBe("warn");
    expect(m?.label).toBe(citedByLabel(claims, false));
    expect(m?.label).toContain("path no longer present");
  });
});

describe("citedByNote (OL-SIBL3)", () => {
  const ok: LiveLookup = { state: "ok", kbs: ["k"], failed: [] };
  it("distinguishes the three absences", () => {
    expect(citedByNote(0, ok)?.kind).toBe("none");
    expect(citedByNote(0, { state: "off", kbs: [], failed: [] })?.kind).toBe("no-pinned");
    expect(citedByNote(0, { state: "failed", kbs: ["k"], failed: [{ kb: "k", reason: "timeout" }] })?.kind).toBe("unreachable");
    expect(citedByNote(0, { state: "failed", kbs: ["k"], failed: [{ kb: "k", reason: "sibling_mismatch" }] })?.kind).toBe("mismatch");
  });
  it("is silent when claims exist and every kb answered, or the lookup is off", () => {
    expect(citedByNote(2, ok)).toBeNull();
    expect(citedByNote(2, { state: "off", kbs: [], failed: [] })).toBeNull();
  });
  it("warns about an incomplete list when claims exist but a kb failed", () => {
    const n = citedByNote(1, { state: "partial", kbs: ["a", "b"], failed: [{ kb: "b", reason: "unreachable" }] });
    expect(n?.kind).toBe("unreachable");
    expect(n?.text).toContain("incomplete");
  });
});
