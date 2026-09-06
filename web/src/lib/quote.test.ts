import { describe, expect, it } from "vitest";
import type { Anchor } from "../api/client";
import {
  QUOTE_MAX_CHARS,
  buildCiteMarkdown,
  buildCiteUrl,
  buildProvenanceBlock,
  buildSelectionCite,
  buildSelectionCiteUrl,
  normalizeQuote,
  renderRegister,
  sessionFocusHref,
} from "./quote";
import type { Ref } from "./registers";

const ORIGIN = "https://kb.example";

describe("normalizeQuote", () => {
  it("returns short text unchanged", () => {
    expect(normalizeQuote("short and sweet")).toBe("short and sweet");
  });
  it("collapses newlines and runs of whitespace to single spaces", () => {
    expect(normalizeQuote("line one\nline two   with   spaces\n")).toBe(
      "line one line two with spaces",
    );
  });
  it("truncates text longer than 200 chars with a trailing ellipsis", () => {
    const long = "x".repeat(250);
    const out = normalizeQuote(long);
    expect(out).toBe(`${"x".repeat(QUOTE_MAX_CHARS)}…`);
    expect(out.length).toBe(QUOTE_MAX_CHARS + 1);
  });
  it("collapses whitespace before truncating (newlines mid-quote, over 200 chars)", () => {
    const long = "para one\nhas\n\nnewlines and   extra   spaces " + "x".repeat(250);
    const out = normalizeQuote(long);
    expect(out).not.toContain("\n");
    expect(out.endsWith("…")).toBe(true);
    expect(out.length).toBe(QUOTE_MAX_CHARS + 1);
  });
  it("keeps text of exactly 200 chars verbatim (boundary, no ellipsis)", () => {
    const at = "y".repeat(QUOTE_MAX_CHARS);
    expect(normalizeQuote(at)).toBe(at);
  });
});

describe("buildCiteUrl", () => {
  it("section anchor: carries ?sec= plus panel/comment, and the text fragment", () => {
    const anchor: Anchor = { kind: "section", id: "getting-started", tag: null, snippet: null };
    const url = buildCiteUrl({
      kb: "docs",
      sourceRelative: "guide/intro.html",
      title: "Intro",
      anchor,
      commentId: "c1",
      body: "worth reading",
      origin: ORIGIN,
    });
    expect(url).toBe(
      "https://kb.example/a/docs/guide/intro.html?sec=getting-started&panel=comments&comment=c1#:~:text=worth%20reading",
    );
  });

  it("selection anchor: omits ?sec= (still opens the panel + jumps to the comment)", () => {
    const anchor: Anchor = { kind: "selection", css_path: "p:nth-child(2)", offset: 4, snippet: "hello" };
    const url = buildCiteUrl({
      kb: "docs",
      sourceRelative: "a/b.html",
      title: "B",
      anchor,
      commentId: "c9",
      body: "note",
      origin: ORIGIN,
    });
    expect(url).toBe(
      "https://kb.example/a/docs/a/b.html?panel=comments&comment=c9#:~:text=note",
    );
  });

  it("file anchor: no ?sec=, percent-encodes the comment id and kb/path segments", () => {
    const url = buildCiteUrl({
      kb: "my kb",
      sourceRelative: "a b/c.html",
      title: "T",
      anchor: { kind: "file" },
      commentId: "c 1",
      body: "x",
      origin: ORIGIN,
    });
    expect(url).toBe(
      "https://kb.example/a/my%20kb/a%20b/c.html?panel=comments&comment=c%201#:~:text=x",
    );
  });

  it("percent-encodes the text-fragment prefix (spaces + punctuation)", () => {
    const url = buildCiteUrl({
      kb: "k",
      sourceRelative: "f.html",
      title: "T",
      anchor: { kind: "file" },
      commentId: "c1",
      body: 'quote: "needs encoding" & more',
      origin: ORIGIN,
    });
    expect(url).toContain(
      "#:~:text=quote%3A%20%22needs%20encoding%22%20%26%20more",
    );
  });

  it("omits the fragment entirely for a blank body", () => {
    const url = buildCiteUrl({
      kb: "k",
      sourceRelative: "f.html",
      title: "T",
      anchor: { kind: "file" },
      commentId: "c1",
      body: "   ",
      origin: ORIGIN,
    });
    expect(url).toBe("https://kb.example/a/k/f.html?panel=comments&comment=c1");
  });
});

describe("buildCiteMarkdown", () => {
  it("formats a blockquote + attribution line, one '§' (no doubling for section anchors)", () => {
    const md = buildCiteMarkdown({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A Doc",
      anchor: { kind: "section", id: "sec1", tag: null, snippet: null },
      commentId: "c1",
      body: "a good point",
      origin: ORIGIN,
    });
    expect(md).toBe(
      "> a good point\n— [A Doc § sec1](https://kb.example/a/docs/a.html?sec=sec1&panel=comments&comment=c1#:~:text=a%20good%20point)",
    );
  });

  it("uses the bare selection quote as the anchor label (no glyph collision)", () => {
    const md = buildCiteMarkdown({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A Doc",
      anchor: { kind: "selection", css_path: "p", offset: 0, snippet: "the quick brown fox jumps over" },
      commentId: "c1",
      body: "agreed",
      origin: ORIGIN,
    });
    expect(md).toContain("— [A Doc § “the quick brown fox ju…”](");
  });
});

describe("buildSelectionCiteUrl", () => {
  const anchor: Anchor = {
    kind: "selection",
    css_path: "p:nth-child(2)",
    offset: 4,
    snippet: "the fox jumps",
  };

  it("carries ?sec= when a nearest section is known, plus the text fragment", () => {
    const url = buildSelectionCiteUrl({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A",
      anchor,
      sec: "getting-started",
      origin: ORIGIN,
    });
    expect(url).toBe(
      "https://kb.example/a/docs/a.html?sec=getting-started#:~:text=the%20fox%20jumps",
    );
  });

  it("omits ?sec= (and any comment/panel param) when no section is known", () => {
    const url = buildSelectionCiteUrl({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A",
      anchor,
      origin: ORIGIN,
    });
    expect(url).toBe("https://kb.example/a/docs/a.html#:~:text=the%20fox%20jumps");
    expect(url).not.toContain("panel=comments");
    expect(url).not.toContain("comment=");
  });

  it("also omits ?sec= for an explicit null section (same as undefined)", () => {
    const url = buildSelectionCiteUrl({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A",
      anchor,
      sec: null,
      origin: ORIGIN,
    });
    expect(url).toBe("https://kb.example/a/docs/a.html#:~:text=the%20fox%20jumps");
  });

  it("percent-encodes the text-fragment prefix", () => {
    const a: Anchor = {
      kind: "selection",
      css_path: "p",
      offset: 0,
      snippet: 'quote: "needs encoding" & more',
    };
    const url = buildSelectionCiteUrl({
      kb: "k",
      sourceRelative: "f.html",
      title: "T",
      anchor: a,
      origin: ORIGIN,
    });
    expect(url).toContain(
      "#:~:text=quote%3A%20%22needs%20encoding%22%20%26%20more",
    );
  });

  it("omits the fragment entirely for a blank snippet", () => {
    const a: Anchor = { kind: "selection", css_path: "p", offset: 0, snippet: "   " };
    const url = buildSelectionCiteUrl({
      kb: "k",
      sourceRelative: "f.html",
      title: "T",
      anchor: a,
      sec: "intro",
      origin: ORIGIN,
    });
    expect(url).toBe("https://kb.example/a/k/f.html?sec=intro");
  });
});

describe("buildSelectionCite", () => {
  it("formats a blockquote + attribution using the quoted snippet as the label (no comment id)", () => {
    const anchor: Anchor = {
      kind: "selection",
      css_path: "p",
      offset: 0,
      snippet: "the quick brown fox jumps over",
    };
    const md = buildSelectionCite({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A Doc",
      anchor,
      sec: "intro",
      origin: ORIGIN,
    });
    expect(md).toBe(
      "> the quick brown fox jumps over\n— [A Doc § “the quick brown fox ju…”](https://kb.example/a/docs/a.html?sec=intro#:~:text=the%20quick%20brown%20fox%20jumps%20over)",
    );
  });

  it("still produces a bare permalink (no ?sec=) when no section is known", () => {
    const anchor: Anchor = { kind: "selection", css_path: "p", offset: 0, snippet: "short" };
    const md = buildSelectionCite({
      kb: "docs",
      sourceRelative: "a.html",
      title: "A Doc",
      anchor,
      origin: ORIGIN,
    });
    expect(md).toContain("(https://kb.example/a/docs/a.html#:~:text=short)");
  });
});

describe("buildProvenanceBlock", () => {
  it("emits title, permalink, and id — one line each — with no session line when unknown", () => {
    const block = buildProvenanceBlock({
      title: "A Doc",
      kb: "docs",
      sourceRelative: "a/b.html",
      id: "abc123",
      origin: ORIGIN,
    });
    expect(block).toBe(
      "A Doc\nhttps://kb.example/a/docs/a/b.html\nid: abc123",
    );
  });

  it("appends the origin session line when a session id is known", () => {
    const block = buildProvenanceBlock({
      title: "A Doc",
      kb: "docs",
      sourceRelative: "a/b.html",
      id: "abc123",
      sessionId: "sess-789",
      origin: ORIGIN,
    });
    expect(block).toBe(
      "A Doc\nhttps://kb.example/a/docs/a/b.html\nid: abc123\norigin session: sess-789",
    );
  });

  it("omits the session line for null/undefined rather than printing it literally", () => {
    expect(
      buildProvenanceBlock({
        title: "A Doc",
        kb: "docs",
        sourceRelative: "a.html",
        id: "x",
        sessionId: null,
        origin: ORIGIN,
      }),
    ).not.toContain("origin session");
  });

  it("percent-encodes kb/path segments the same way artifactHref does", () => {
    const block = buildProvenanceBlock({
      title: "T",
      kb: "my kb",
      sourceRelative: "a b/c.html",
      id: "x",
      origin: ORIGIN,
    });
    expect(block).toContain("https://kb.example/a/my%20kb/a%20b/c.html");
  });

  // W3.P-c generalized the two optional fields; the `y p` shape above is
  // unchanged, and these pin the new degradations.
  it("omits the id line entirely when no artifact id is known (a migrated W2.6b mark)", () => {
    expect(
      buildProvenanceBlock({
        title: "A Doc",
        kb: "docs",
        sourceRelative: "a.html",
        id: null,
        origin: ORIGIN,
      }),
    ).toBe("A Doc\nhttps://kb.example/a/docs/a.html");
  });

  it("rides artifactHref's ?sec= grammar when a section was captured", () => {
    expect(
      buildProvenanceBlock({
        title: "A Doc",
        kb: "docs",
        sourceRelative: "a.html",
        id: "x",
        sec: "intro",
        origin: ORIGIN,
      }),
    ).toBe("A Doc\nhttps://kb.example/a/docs/a.html?sec=intro\nid: x");
  });
});

// W3.P-c — provenance registers render through THIS module, so the paste
// payload of a register is the SAME grammar as the `y p` yank and the
// comment/selection cite. Golden-pinned per kind.
describe("renderRegister", () => {
  it("artifact — the provenance block, with the captured section on the permalink", () => {
    const ref: Ref = {
      kind: "artifact",
      kb: "docs",
      sourceRelative: "a/b.html",
      title: "A Doc",
      sec: "intro",
      id: "abc123",
      sessionId: "sess-789",
    };
    expect(renderRegister(ref, ORIGIN)).toBe(
      "A Doc\nhttps://kb.example/a/docs/a/b.html?sec=intro\nid: abc123\norigin session: sess-789",
    );
  });

  it("artifact — a mark migrated forward from W2.6b (no id, no session, no section)", () => {
    const ref: Ref = {
      kind: "artifact",
      kb: "docs",
      sourceRelative: "a/b.html",
      title: "A Doc",
      sec: null,
      id: null,
    };
    expect(renderRegister(ref, ORIGIN)).toBe("A Doc\nhttps://kb.example/a/docs/a/b.html");
  });

  it("artifact — a blank title falls back to the source-relative path", () => {
    const ref: Ref = {
      kind: "artifact",
      kb: "docs",
      sourceRelative: "a/b.html",
      title: "",
      sec: null,
    };
    expect(renderRegister(ref, ORIGIN).split("\n")[0]).toBe("a/b.html");
  });

  it("artifact — byte-identical to the `y p` yank for the same artifact (ONE grammar)", () => {
    const shared = {
      title: "A Doc",
      kb: "docs",
      sourceRelative: "a/b.html",
      id: "abc123",
      sessionId: "sess-789",
    };
    expect(renderRegister({ kind: "artifact", ...shared, sec: null }, ORIGIN)).toBe(
      buildProvenanceBlock({ ...shared, origin: ORIGIN }),
    );
  });

  it("selection — the highlight citation block, verbatim from buildSelectionCite", () => {
    const ref: Ref = {
      kind: "selection",
      kb: "docs",
      sourceRelative: "a.html",
      title: "A Doc",
      sec: "sec-2",
      cssPath: "main > p:nth-of-type(2)",
      offset: 12,
      snippet: "a quoted phrase",
    };
    expect(renderRegister(ref, ORIGIN)).toBe(
      buildSelectionCite({
        kb: "docs",
        sourceRelative: "a.html",
        title: "A Doc",
        anchor: {
          kind: "selection",
          css_path: "main > p:nth-of-type(2)",
          offset: 12,
          snippet: "a quoted phrase",
        },
        sec: "sec-2",
        origin: ORIGIN,
      }),
    );
    expect(renderRegister(ref, ORIGIN)).toContain("> a quoted phrase\n— [A Doc");
  });

  it("session — label, the /sessions?focus= deep link, and the canonical id", () => {
    const ref: Ref = {
      kind: "session",
      sessionId: "sess 789/abc",
      title: "session sess 789",
    };
    expect(renderRegister(ref, ORIGIN)).toBe(
      "session sess 789\nhttps://kb.example/sessions?focus=sess%20789%2Fabc\nsession: sess 789/abc",
    );
    expect(sessionFocusHref("sess 789/abc")).toBe("/sessions?focus=sess%20789%2Fabc");
  });

  it("session — a blank title falls back to the session id", () => {
    expect(
      renderRegister({ kind: "session", sessionId: "s1", title: "" }, ORIGIN).split("\n")[0],
    ).toBe("s1");
  });

  it("commit — subject + sha, no invented forge URL, repo only when known", () => {
    expect(
      renderRegister({ kind: "commit", sha: "0f722cb4", subject: "feat: the thing" }, ORIGIN),
    ).toBe("feat: the thing\ncommit: 0f722cb4");
    expect(
      renderRegister(
        { kind: "commit", sha: "0f722cb4", subject: "feat: the thing", repo: "kb" },
        ORIGIN,
      ),
    ).toBe("feat: the thing\ncommit: 0f722cb4\nrepo: kb");
    expect(renderRegister({ kind: "commit", sha: "0f722cb4", subject: "" }, ORIGIN)).toBe(
      "0f722cb4\ncommit: 0f722cb4",
    );
  });
});
