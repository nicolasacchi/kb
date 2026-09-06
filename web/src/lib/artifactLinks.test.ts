import { describe, expect, it } from "vitest";
import {
  parseArtifactIdFromHost,
  resolveArtifactHref,
  type ArtifactLinkCtx,
} from "./artifactLinks";

// The link grammar is golden-pinned the way `paneUrl` / `galleryUrl` are:
// one table per shape, exact objects (never `toMatchObject`), so a future
// refactor cannot quietly change what a hovered/clicked artifact link
// resolves to. The resolver is TOTAL — every row below asserts one of the
// four cases, and "I can't name this honestly" is always `external`.

const CTX: ArtifactLinkCtx = {
  paneKb: "canon",
  paneSourceRelative: "pm/00-summary.html",
  paneId: "aaaaaaaaaaaa",
  hostSuffix: ".artifacts.localhost",
  spaOrigin: "http://127.0.0.1:4737",
  kbIds: ["canon", "research"],
};

const ROOT_CTX: ArtifactLinkCtx = {
  ...CTX,
  paneSourceRelative: "kitchen-sink.html",
};

describe("parseArtifactIdFromHost — the client mirror of kb_core::iframe (bare fallthrough)", () => {
  it("takes the first label when the remainder is exactly the suffix", () => {
    expect(
      parseArtifactIdFromHost("abc123def456.artifacts.localhost", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "abc123def456" });
  });

  it("is case-insensitive on the host", () => {
    expect(
      parseArtifactIdFromHost("ABC123DEF456.Artifacts.LocalHost", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "abc123def456" });
  });

  it("strips a port defensively (URL.hostname never carries one)", () => {
    expect(
      parseArtifactIdFromHost("abc123def456.artifacts.localhost:4737", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "abc123def456" });
  });

  it("rejects a bare suffix (empty id)", () => {
    expect(parseArtifactIdFromHost("artifacts.localhost", ".artifacts.localhost")).toBeNull();
  });

  it("rejects a host that merely ends with a similar string", () => {
    expect(parseArtifactIdFromHost("evil.com", ".artifacts.localhost")).toBeNull();
    expect(
      parseArtifactIdFromHost("abc.artifacts.localhost.evil.com", ".artifacts.localhost"),
    ).toBeNull();
  });

  it("rejects traversal-shaped ids (leading/trailing dot, '..')", () => {
    expect(parseArtifactIdFromHost("..artifacts.localhost", ".artifacts.localhost")).toBeNull();
    expect(parseArtifactIdFromHost("a..b.artifacts.localhost", ".artifacts.localhost")).toBeNull();
  });
});

describe("parseArtifactIdFromHost — ARTIFACT HOST GRAMMAR v2 (qualified label)", () => {
  it("splits kb_enc from a 12-hex id at the '--'", () => {
    expect(
      parseArtifactIdFromHost(
        "canon--0eb547304658.artifacts.localhost",
        ".artifacts.localhost",
      ),
    ).toEqual({ kind: "qualified", kbEnc: "canon", id: "0eb547304658" });
  });

  it("is case-insensitive on both kb_enc and id", () => {
    expect(
      parseArtifactIdFromHost(
        "CANON--0EB547304658.Artifacts.LocalHost",
        ".artifacts.localhost",
      ),
    ).toEqual({ kind: "qualified", kbEnc: "canon", id: "0eb547304658" });
  });

  it("carries a kb_enc that already encodes an underscored kb name as dashes", () => {
    // The daemon/SPA always encode '_' -> '-' before building the label
    // (encodeKbForHost); the parser just extracts kb_enc verbatim.
    expect(
      parseArtifactIdFromHost(
        "obs-docs--0eb547304658.artifacts.example.com",
        ".artifacts.example.com",
      ),
    ).toEqual({ kind: "qualified", kbEnc: "obs-docs", id: "0eb547304658" });
  });

  it("splits at the RIGHTMOST '--' when kb_enc itself contains one", () => {
    expect(
      parseArtifactIdFromHost(
        "foo--bar--0eb547304658.artifacts.localhost",
        ".artifacts.localhost",
      ),
    ).toEqual({ kind: "qualified", kbEnc: "foo--bar", id: "0eb547304658" });
  });

  it("falls through to bare when the id half isn't exactly 12 hex chars", () => {
    expect(
      parseArtifactIdFromHost("canon--tooshort.artifacts.localhost", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "canon--tooshort" });
    expect(
      parseArtifactIdFromHost(
        "canon--0eb5473046580.artifacts.localhost",
        ".artifacts.localhost",
      ),
    ).toEqual({ kind: "bare", id: "canon--0eb5473046580" });
  });

  it("falls through to bare when the prefix before '--' is empty", () => {
    expect(
      parseArtifactIdFromHost("--0eb547304658.artifacts.localhost", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "--0eb547304658" });
  });

  it("falls through to bare when kb_enc carries an UNENCODED underscore", () => {
    // '_' is not in the kb_enc grammar [a-z0-9][a-z0-9-]* — a real link
    // always carries the encoded '-' form; a raw '_' here is not a
    // qualified label at all.
    expect(
      parseArtifactIdFromHost(
        "obs_docs--0eb547304658.artifacts.example.com",
        ".artifacts.example.com",
      ),
    ).toEqual({ kind: "bare", id: "obs_docs--0eb547304658" });
  });

  it("falls through to bare when kb_enc starts with '-'", () => {
    expect(
      parseArtifactIdFromHost(
        "-badenc--0eb547304658.artifacts.localhost",
        ".artifacts.localhost",
      ),
    ).toEqual({ kind: "bare", id: "-badenc--0eb547304658" });
  });

  it("a bare 12-hex label with no '--' prefix stays bare (no kb_enc at all)", () => {
    expect(
      parseArtifactIdFromHost("0eb547304658.artifacts.localhost", ".artifacts.localhost"),
    ).toEqual({ kind: "bare", id: "0eb547304658" });
  });
});

describe("resolveArtifactHref — the /a/<kb>/<path> permalink form", () => {
  it("resolves a permalink on the SPA origin", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/canon/pm/02-cause.html", CTX),
    ).toEqual({ kind: "artifact", kb: "canon", sourceRelative: "pm/02-cause.html" });
  });

  it("resolves a CROSS-KB permalink", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/research/deep/dive.html", CTX),
    ).toEqual({ kind: "artifact", kb: "research", sourceRelative: "deep/dive.html" });
  });

  it("honours a permalink hardcoded inside an artifact (resolved against ITS origin)", () => {
    expect(
      resolveArtifactHref(
        "http://aaaaaaaaaaaa.artifacts.localhost:4737/a/canon/pm/03-actions.html",
        CTX,
      ),
    ).toEqual({ kind: "artifact", kb: "canon", sourceRelative: "pm/03-actions.html" });
  });

  it("carries the fragment through as `sec`", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/canon/pm/02-cause.html#why-pin", CTX),
    ).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/02-cause.html",
      sec: "why-pin",
    });
  });

  it("decodes percent-escaped path segments and fragments", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/canon/pm/a%20b.html#s%C3%A9c", CTX),
    ).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/a b.html",
      sec: "séc",
    });
  });

  it("returns artifact-id for a bare 12-hex id in the path slot", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/canon/0123456789ab", CTX),
    ).toEqual({ kind: "artifact-id", kb: "canon", id: "0123456789ab" });
  });

  it("treats a permalink naming the pane's OWN artifact as same-doc", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/canon/pm/00-summary.html", CTX),
    ).toEqual({ kind: "same-doc" });
  });

  it("rejects a permalink naming a kb this daemon doesn't serve", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/nope/x.html", CTX),
    ).toEqual({ kind: "external" });
  });

  it("trusts the kb segment when the kb list hasn't loaded yet", () => {
    expect(
      resolveArtifactHref("http://127.0.0.1:4737/a/nope/x.html", { ...CTX, kbIds: [] }),
    ).toEqual({ kind: "artifact", kb: "nope", sourceRelative: "x.html" });
  });

  it("refuses the permalink SHAPE on a third-party origin", () => {
    expect(
      resolveArtifactHref("https://evil.example/a/canon/pm/02-cause.html", CTX),
    ).toEqual({ kind: "external" });
  });

  it("rejects a truncated permalink (no path after the kb)", () => {
    expect(resolveArtifactHref("http://127.0.0.1:4737/a/canon", CTX)).toEqual({
      kind: "external",
    });
  });
});

describe("resolveArtifactHref — artifact-subdomain URLs", () => {
  it("names a FOREIGN artifact origin by id (no kb in the URL)", () => {
    expect(
      resolveArtifactHref("http://0123456789ab.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "artifact-id", kb: null, id: "0123456789ab" });
  });

  it("keeps the fragment on the id form", () => {
    expect(
      resolveArtifactHref("http://0123456789ab.artifacts.localhost:4737/#intro", CTX),
    ).toEqual({ kind: "artifact-id", kb: null, id: "0123456789ab", sec: "intro" });
  });

  it("works without a port", () => {
    expect(
      resolveArtifactHref("https://0123456789ab.artifacts.example.com/", {
        ...CTX,
        hostSuffix: ".artifacts.example.com",
        spaOrigin: "https://kb.example.com",
      }),
    ).toEqual({ kind: "artifact-id", kb: null, id: "0123456789ab" });
  });

  it("resolves a path on the pane's OWN origin against the pane's directory", () => {
    // This is the shape the runtime relay actually posts: a relative link
    // inside `pm/00-summary.html`, absolutised against the iframe's own
    // location (`<own-id>.artifacts.localhost/01-timeline.html`).
    expect(
      resolveArtifactHref(
        "http://aaaaaaaaaaaa.artifacts.localhost:4737/01-timeline.html",
        CTX,
      ),
    ).toEqual({ kind: "artifact", kb: "canon", sourceRelative: "pm/01-timeline.html" });
  });

  it("treats the pane's own origin root as same-doc", () => {
    expect(
      resolveArtifactHref("http://aaaaaaaaaaaa.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "same-doc" });
  });

  it("refuses a sub-path under a FOREIGN artifact origin (an asset we can't name)", () => {
    expect(
      resolveArtifactHref(
        "http://0123456789ab.artifacts.localhost:4737/style.css",
        CTX,
      ),
    ).toEqual({ kind: "external" });
  });

  it("treats any same-suffix origin as the pane's own when paneId is unknown", () => {
    expect(
      resolveArtifactHref("http://0123456789ab.artifacts.localhost:4737/x.html", {
        ...CTX,
        paneId: null,
      }),
    ).toEqual({ kind: "artifact", kb: "canon", sourceRelative: "pm/x.html" });
  });
});

describe("resolveArtifactHref — ARTIFACT HOST GRAMMAR v2 (qualified subdomain URLs)", () => {
  it("treats a qualified label naming the pane's OWN kb+id as same-doc", () => {
    expect(
      resolveArtifactHref("http://canon--aaaaaaaaaaaa.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "same-doc" });
  });

  it("resolves a qualified label naming this pane's kb but a DIFFERENT id, carrying the kb", () => {
    expect(
      resolveArtifactHref("http://canon--bbbbbbbbbbbb.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "artifact-id", kb: "canon", id: "bbbbbbbbbbbb" });
  });

  it("resolves a qualified label naming a DIFFERENT known kb by matching kb_enc uniquely", () => {
    expect(
      resolveArtifactHref("http://research--0123456789ab.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "artifact-id", kb: "research", id: "0123456789ab" });
  });

  it("keeps the fragment on a qualified id form", () => {
    expect(
      resolveArtifactHref(
        "http://research--0123456789ab.artifacts.localhost:4737/#intro",
        CTX,
      ),
    ).toEqual({ kind: "artifact-id", kb: "research", id: "0123456789ab", sec: "intro" });
  });

  it("falls back to kb:null when kb_enc names no kb this daemon serves", () => {
    expect(
      resolveArtifactHref("http://nope--0123456789ab.artifacts.localhost:4737/", CTX),
    ).toEqual({ kind: "artifact-id", kb: null, id: "0123456789ab" });
  });

  it("falls back to kb:null on an AMBIGUOUS kb_enc match (two kb names encode identically)", () => {
    expect(
      resolveArtifactHref("http://a-b--0123456789ab.artifacts.localhost:4737/", {
        ...CTX,
        kbIds: ["a_b", "a-b"],
      }),
    ).toEqual({ kind: "artifact-id", kb: null, id: "0123456789ab" });
  });

  it("resolves a path under the pane's OWN qualified origin against the pane's directory", () => {
    expect(
      resolveArtifactHref(
        "http://canon--aaaaaaaaaaaa.artifacts.localhost:4737/01-timeline.html",
        CTX,
      ),
    ).toEqual({ kind: "artifact", kb: "canon", sourceRelative: "pm/01-timeline.html" });
  });

  it("refuses a sub-path under a FOREIGN kb's qualified origin, even sharing this pane's id", () => {
    // Same 12-hex id as the pane's own — the '--research' kb qualifier is
    // what makes this a DIFFERENT artifact, and a sub-path under it is an
    // asset we cannot honestly name (mirrors the bare foreign-origin case).
    expect(
      resolveArtifactHref(
        "http://research--aaaaaaaaaaaa.artifacts.localhost:4737/style.css",
        CTX,
      ),
    ).toEqual({ kind: "external" });
  });
});

describe("resolveArtifactHref — relative hrefs", () => {
  it("resolves a sibling file against the pane's directory", () => {
    expect(resolveArtifactHref("01-timeline.html", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/01-timeline.html",
    });
  });

  it("resolves ./ and one level up", () => {
    expect(resolveArtifactHref("./02-cause.html", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/02-cause.html",
    });
    expect(resolveArtifactHref("../kitchen-sink.html", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "kitchen-sink.html",
    });
  });

  it("resolves a descendant path", () => {
    expect(resolveArtifactHref("sub/dir/deep.html", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/sub/dir/deep.html",
    });
  });

  it("treats a root-absolute path as relative to the artifact's own directory", () => {
    // The artifact origin serves the artifact's directory at `/`, so
    // `/01-timeline.html` means the same thing `01-timeline.html` does.
    expect(resolveArtifactHref("/01-timeline.html", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/01-timeline.html",
    });
  });

  it("REJECTS a traversal escape above the kb root (the URL API would clamp it)", () => {
    expect(resolveArtifactHref("../../secrets.html", CTX)).toEqual({
      kind: "external",
    });
    expect(resolveArtifactHref("../../../../etc/passwd", CTX)).toEqual({
      kind: "external",
    });
  });

  it("rejects a '..' from a root-level artifact", () => {
    expect(resolveArtifactHref("../x.html", ROOT_CTX)).toEqual({
      kind: "external",
    });
  });

  it("treats a bare fragment as same-doc", () => {
    expect(resolveArtifactHref("#findings", CTX)).toEqual({ kind: "same-doc" });
  });

  it("treats a relative href back to the pane's own file as same-doc", () => {
    expect(resolveArtifactHref("00-summary.html#findings", CTX)).toEqual({
      kind: "same-doc",
    });
  });

  it("carries the fragment on a relative link", () => {
    expect(resolveArtifactHref("01-timeline.html#t-0900", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/01-timeline.html",
      sec: "t-0900",
    });
  });

  it("drops a query string (not part of the source-path grammar)", () => {
    expect(resolveArtifactHref("01-timeline.html?v=2#top", CTX)).toEqual({
      kind: "artifact",
      kb: "canon",
      sourceRelative: "pm/01-timeline.html",
      sec: "top",
    });
  });
});

describe("resolveArtifactHref — never an artifact", () => {
  it("rejects an empty / whitespace href", () => {
    expect(resolveArtifactHref("", CTX)).toEqual({ kind: "external" });
    expect(resolveArtifactHref("   ", CTX)).toEqual({ kind: "external" });
  });

  it("rejects non-http(s) schemes", () => {
    for (const href of [
      "mailto:alice@example.com",
      "javascript:alert(1)",
      "data:text/html,<b>x</b>",
      "tel:+123456",
    ]) {
      expect(resolveArtifactHref(href, CTX), href).toEqual({ kind: "external" });
    }
  });

  it("rejects an off-daemon http URL", () => {
    expect(resolveArtifactHref("https://example.com/blog/post", CTX)).toEqual({
      kind: "external",
    });
  });

  it("rejects a non-permalink path on the SPA origin", () => {
    expect(resolveArtifactHref("http://127.0.0.1:4737/search?q=x", CTX)).toEqual({
      kind: "external",
    });
  });
});
