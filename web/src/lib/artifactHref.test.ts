import { describe, it, expect } from "vitest";
import { artifactHref } from "./artifactHref";

describe("artifactHref", () => {
  it("builds /a/<kb>/<rel> preserving the path separators", () => {
    expect(artifactHref("platform", "ideas/foo/bar.html")).toBe("/a/platform/ideas/foo/bar.html");
  });
  it("percent-encodes each segment individually (slashes stay literal)", () => {
    expect(artifactHref("kb", "a b/c#d.html")).toBe("/a/kb/a%20b/c%23d.html");
  });
  it("encodes the kb name", () => {
    expect(artifactHref("my kb", "x.html")).toBe("/a/my%20kb/x.html");
  });
  it("appends an encoded ?p= page param", () => {
    expect(artifactHref("kb", "x.html", "page two")).toBe("/a/kb/x.html?p=page%20two");
  });
  it("omits ?p= when page is undefined", () => {
    expect(artifactHref("kb", "x.html")).toBe("/a/kb/x.html");
  });
  it("omits ?p= for an empty-string page (falsy)", () => {
    expect(artifactHref("kb", "x.html", "")).toBe("/a/kb/x.html");
  });
  it("encodes query-significant chars in a segment (encodeURIComponent, not encodeURI)", () => {
    // encodeURI would leave ?, &, # bare; encodeURIComponent escapes them,
    // keeping them literal path components rather than a query/fragment.
    expect(artifactHref("kb", "a?b&c.html")).toBe("/a/kb/a%3Fb%26c.html");
  });
  it("accepts an options bag with page (back-compatible with positional)", () => {
    expect(artifactHref("kb", "x.html", { page: "page two" })).toBe(
      "/a/kb/x.html?p=page%20two",
    );
  });
  it("appends ?sec= for a section deep link", () => {
    expect(artifactHref("kb", "x.html", { sec: "why-pin" })).toBe(
      "/a/kb/x.html?sec=why-pin",
    );
  });
  it("composes page + sec + trail params in stable order", () => {
    expect(
      artifactHref("kb", "x.html", {
        page: "ch2.html",
        sec: "intro",
        list: "l_9f3a21c4d0aa",
        entry: "le_02bd11aa34f0",
      }),
    ).toBe("/a/kb/x.html?p=ch2.html&sec=intro&list=l_9f3a21c4d0aa&entry=le_02bd11aa34f0");
  });
  it("skips empty opt values", () => {
    expect(artifactHref("kb", "x.html", { sec: "", list: undefined })).toBe(
      "/a/kb/x.html",
    );
  });
  it("percent-encodes section ids with special chars", () => {
    expect(artifactHref("kb", "x.html", { sec: "a b&c" })).toBe(
      "/a/kb/x.html?sec=a%20b%26c",
    );
  });
  it("appends ?panel=comments for an inbox deep-link", () => {
    expect(artifactHref("kb", "x.html", { panel: "comments" })).toBe(
      "/a/kb/x.html?panel=comments",
    );
  });
  // W3.P-b — `pane2` is appended LAST so every golden string above stays
  // byte-identical when it's absent (the tests above are the proof).
  it("appends ?pane2= for the two-pane split", () => {
    expect(
      artifactHref("canon", "kitchen-sink.html", { pane2: "canon:multi-page.html" }),
    ).toBe("/a/canon/kitchen-sink.html?pane2=canon%3Amulti-page.html");
  });
  it("puts pane2 after every other param", () => {
    expect(
      artifactHref("kb", "x.html", {
        page: "ch2.html",
        sec: "intro",
        list: "l_9f3a21c4d0aa",
        entry: "le_02bd11aa34f0",
        panel: "comments",
        pane2: "kb:y.html",
      }),
    ).toBe(
      "/a/kb/x.html?p=ch2.html&sec=intro&list=l_9f3a21c4d0aa&entry=le_02bd11aa34f0&panel=comments&pane2=kb%3Ay.html",
    );
  });
  it("omits ?pane2= for an empty-string value (falsy)", () => {
    expect(artifactHref("kb", "x.html", { pane2: "" })).toBe("/a/kb/x.html");
  });

  // W3.E/S5 — `?turn=` accepts a numeric ordinal or the literal "end", and is
  // serialised BEFORE `pane2` (which stays last, byte-compat per #30 v0.29).
  it("appends ?turn= for a numeric turn deep link", () => {
    expect(artifactHref("kb", "x.html", { turn: 12 })).toBe(
      "/a/kb/x.html?turn=12",
    );
  });
  it("appends ?turn=end for the outcome jump", () => {
    expect(artifactHref("kb", "x.html", { turn: "end" })).toBe(
      "/a/kb/x.html?turn=end",
    );
  });
  it("keeps ?turn= before ?pane2= when both are set", () => {
    expect(
      artifactHref("kb", "x.html", { turn: "end", pane2: "kb:y.html" }),
    ).toBe("/a/kb/x.html?turn=end&pane2=kb%3Ay.html");
  });
  it("distinguishes turn=0 from absent (a valid ordinal, not falsy-omitted)", () => {
    expect(artifactHref("kb", "x.html", { turn: 0 })).toBe(
      "/a/kb/x.html?turn=0",
    );
  });
  it("accepts a t-<uuid12> turn id verbatim (W7/LF-4 handoff)", () => {
    expect(artifactHref("kb", "x.html", { turn: "t-0123456789ab" })).toBe(
      "/a/kb/x.html?turn=t-0123456789ab",
    );
  });

  // W7 (R15/LF-2) — `?follow=1`, right after `turn`, before `pane2`.
  it("appends ?follow=1 when follow is true", () => {
    expect(artifactHref("kb", "x.html", { follow: true })).toBe(
      "/a/kb/x.html?follow=1",
    );
  });
  it("omits ?follow= when follow is false or absent", () => {
    expect(artifactHref("kb", "x.html", { follow: false })).toBe(
      "/a/kb/x.html",
    );
    expect(artifactHref("kb", "x.html", {})).toBe("/a/kb/x.html");
  });
  it("orders turn before follow before pane2", () => {
    expect(
      artifactHref("kb", "x.html", {
        turn: "end",
        follow: true,
        pane2: "kb:y.html",
      }),
    ).toBe("/a/kb/x.html?turn=end&follow=1&pane2=kb%3Ay.html");
  });

  // CT-F6 — `?at=` (Memento). Every golden ABOVE is the byte-compat proof
  // that adding it changed nothing: `at` only appears when asked for, and
  // `pane2` is still last.
  it("appends ?at= for a memento instant", () => {
    expect(artifactHref("kb", "x.html", { at: 1700000000 })).toBe(
      "/a/kb/x.html?at=1700000000",
    );
  });
  it("omits ?at= when absent", () => {
    expect(artifactHref("kb", "x.html", {})).toBe("/a/kb/x.html");
  });
  it("distinguishes at=0 from absent (the epoch is a valid instant)", () => {
    expect(artifactHref("kb", "x.html", { at: 0 })).toBe("/a/kb/x.html?at=0");
  });
  it("truncates a fractional instant to whole unix seconds", () => {
    expect(artifactHref("kb", "x.html", { at: 1700000000.75 })).toBe(
      "/a/kb/x.html?at=1700000000",
    );
  });
  it("orders follow before at before pane2", () => {
    expect(
      artifactHref("kb", "x.html", {
        turn: "end",
        follow: true,
        at: 1700000000,
        pane2: "kb:y.html",
      }),
    ).toBe("/a/kb/x.html?turn=end&follow=1&at=1700000000&pane2=kb%3Ay.html");
  });
  // CT-F6 — the session-end link shape: open the versions panel AT the
  // instant the session closed.
  it("composes ?panel=versions with ?at= (the 'as it stood at session end' link)", () => {
    expect(
      artifactHref("canon", "kitchen-sink.html", {
        panel: "versions",
        at: 1700000000,
      }),
    ).toBe("/a/canon/kitchen-sink.html?panel=versions&at=1700000000");
  });
});
