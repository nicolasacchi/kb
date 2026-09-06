import { afterEach, describe, expect, it } from "vitest";
import type { LaneSection } from "../api/types";
import {
  LANE_ORDER,
  extractRepoFilter,
  fullSearchUrl,
  laneRowCount,
  orderSections,
  sessionUrl,
  setKbSessionBase,
  transcriptReaderPath,
} from "./searchLanes";

const DEFAULT_BASE = "http://127.0.0.1:4000";

// `kbSessionBase` is module-scoped state shared across every test in this
// file (and, within a worker, every OTHER test file that imports
// `searchLanes.ts`) - reset after every test that calls `setKbSessionBase`
// so the "default" assertions elsewhere in this describe block stay true
// regardless of run order.
afterEach(() => {
  setKbSessionBase(DEFAULT_BASE);
});

function section(lane: LaneSection["lane"], overrides: Partial<LaneSection> = {}): LaneSection {
  return { lane, results: [], truncated: false, ...overrides };
}

describe("orderSections", () => {
  it("reorders a scrambled subset into the canonical lane order", () => {
    const scrambled = [section("transcripts"), section("files"), section("semantic")];
    expect(orderSections(scrambled).map((s) => s.lane)).toEqual(["files", "semantic", "transcripts"]);
  });

  it("is a no-op (stable) on an already-ordered full set", () => {
    const ordered = LANE_ORDER.map((lane) => section(lane));
    expect(orderSections(ordered).map((s) => s.lane)).toEqual(LANE_ORDER);
  });

  it("does not mutate the input array", () => {
    const scrambled = [section("symbols"), section("files")];
    const copy = [...scrambled];
    orderSections(scrambled);
    expect(scrambled).toEqual(copy);
  });
});

describe("sessionUrl", () => {
  it("builds kb's own SPA session URL, percent-encoding the id, against the default base", () => {
    expect(sessionUrl("abc123")).toBe("http://127.0.0.1:4000/sessions/abc123");
    expect(sessionUrl("has space")).toBe("http://127.0.0.1:4000/sessions/has%20space");
  });

  it("reflects a base set via setKbSessionBase (the boot identity-fetch path)", () => {
    setKbSessionBase("https://kb.example.com");
    expect(sessionUrl("abc123")).toBe("https://kb.example.com/sessions/abc123");
  });

  it("trims a trailing slash off the set base so the join never double-slashes", () => {
    setKbSessionBase("https://kb.example.com/");
    expect(sessionUrl("abc123")).toBe("https://kb.example.com/sessions/abc123");
  });

  it("scopes the link with ?kb= when a corpus is given", () => {
    expect(sessionUrl("abc123", "memory")).toBe("http://127.0.0.1:4000/sessions/abc123?kb=memory");
  });

  it("percent-encodes the kb param too", () => {
    expect(sessionUrl("abc123", "a b")).toBe("http://127.0.0.1:4000/sessions/abc123?kb=a%20b");
  });

  it("omits ?kb= when the corpus is absent, byte-identical to the pre-existing link", () => {
    expect(sessionUrl("abc123", undefined)).toBe("http://127.0.0.1:4000/sessions/abc123");
  });
});

describe("transcriptReaderPath", () => {
  it("always returns null - TranscriptHit carries no file/path field today", () => {
    const hit = {
      session_id: "s1",
      uuid: "u1",
      ts: 0,
      kind: "user",
      tool_name: null,
      project_dir: "/tmp",
      snippet: "hello",
      is_sidechain: false,
    };
    expect(transcriptReaderPath(hit)).toBeNull();
  });
});

describe("extractRepoFilter", () => {
  it("finds a repo: token among other words", () => {
    expect(extractRepoFilter("widget repo:kb more words")).toBe("kb");
  });

  it("returns undefined when no repo: token is present", () => {
    expect(extractRepoFilter("just a plain query")).toBeUndefined();
  });

  it("last occurrence wins, mirroring grammar.rs's own rule", () => {
    expect(extractRepoFilter("repo:one repo:two")).toBe("two");
  });

  it("ignores an empty repo: value", () => {
    expect(extractRepoFilter("widget repo:")).toBeUndefined();
  });

  // V71-D1 — now that this goes through the kbcq/1 mirror, it agrees with
  // the daemon on the cases the old token regex got wrong.
  it("a quoted repo: is a search term, not a scope", () => {
    expect(extractRepoFilter('widget "repo:kb"')).toBeUndefined();
  });

  it("a negated repo: is refused (repo: is not negatable), not a scope", () => {
    expect(extractRepoFilter("widget -repo:kb")).toBeUndefined();
  });
});

describe("fullSearchUrl", () => {
  it("builds with only q", () => {
    expect(fullSearchUrl("widget")).toBe("/search?q=widget");
  });

  it("includes repo when given", () => {
    expect(fullSearchUrl("widget", "kb")).toBe("/search?q=widget&repo=kb");
  });

  it("omits the query string entirely for an empty q and no repo", () => {
    expect(fullSearchUrl("")).toBe("/search");
  });
});

describe("laneRowCount", () => {
  it("counts files/symbols/semantic/sessions/transcripts as results.length", () => {
    expect(laneRowCount(section("files", { results: [{}, {}] }))).toBe(2);
    expect(laneRowCount(section("symbols", { results: [{}] }))).toBe(1);
  });

  it("flattens the text lane's per-file matches into a per-match count", () => {
    const results = [
      { path: "a.rs", matches: [{}, {}] },
      { path: "b.rs", matches: [{}] },
    ];
    expect(laneRowCount(section("text", { results }))).toBe(3);
  });

  it("is zero for a pending section regardless of results", () => {
    expect(laneRowCount(section("semantic", { pending: true, results: [{}, {}] }))).toBe(0);
  });

  it("is zero for an unavailable section", () => {
    expect(
      laneRowCount(section("sessions", { unavailable_reason: "q must not be empty" })),
    ).toBe(0);
  });
});
