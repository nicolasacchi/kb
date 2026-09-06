import { describe, it, expect } from "vitest";
import { parseCallout } from "./callout";

// Pins the SPA's native callout parse to the server's `kb_core::markdown`
// callout semantics across the SAME fixtures the Rust suite uses
// (`render_fragment_styles_obsidian_callout`,
// `callout_default_title_is_capitalised_type`,
// `callout_type_alias_kept_verbatim_lowercased`,
// `plain_blockquote_is_not_a_callout`), so the native note view and the
// served-HTML iframe render callouts identically. remark strips the `>`
// upstream, so these are the de-quoted first-line forms (no leading `> `).

describe("parseCallout", () => {
  it("parses an explicit type + title", () => {
    expect(parseCallout("[!warning] Heads up")).toEqual({
      type: "warning",
      title: "Heads up",
    });
  });

  it("defaults the title to the capitalised type", () => {
    expect(parseCallout("[!tip]")).toEqual({ type: "tip", title: "Tip" });
  });

  it("lowercases the type (alias/unknown kept verbatim, lowercased)", () => {
    expect(parseCallout("[!CAUTION] x")).toEqual({
      type: "caution",
      title: "x",
    });
  });

  it("consumes a fold marker flush against ] but keeps a dash title", () => {
    // `[!note]-` fold marker is dropped; a title that starts with `-`
    // (after a space) survives — matches the server's `callout_header`,
    // which only strips `+`/`-` when flush against `]`.
    expect(parseCallout("[!note]- folded")).toEqual({
      type: "note",
      title: "folded",
    });
    expect(parseCallout("[!note] -dashtitle")).toEqual({
      type: "note",
      title: "-dashtitle",
    });
  });

  it("rejects non-callout lines", () => {
    expect(parseCallout("just an ordinary quote")).toBeNull();
    expect(parseCallout("[!] empty type")).toBeNull();
    expect(parseCallout("[!bad type] space in type")).toBeNull();
    // `_` is NOT in the server's type charset (ASCII alnum + dash only),
    // so an underscore type must NOT parse as a callout — else it would
    // render natively but stay a plain blockquote in the iframe.
    expect(parseCallout("[!my_type] x")).toBeNull();
  });
});
