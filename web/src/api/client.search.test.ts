import { describe, it, expect, vi, afterEach } from "vitest";
import { search } from "./client";

// FS1 — the search() URL builder is the wire contract for the faceted
// search page. Two invariants it must hold: (1) every faceted axis
// serialises default-out (csv for arrays), so links stay clean; (2) the
// Cmd+K popup's positional call `search(q, mode, kb)` is byte-identical to
// pre-Q-track — no scope/limit/detail/facet params leak onto its wire.

function mockFetch(): string[] {
  const calls: string[] = [];
  const fn = vi.fn(async (url: string | URL) => {
    calls.push(String(url));
    return new Response(
      JSON.stringify({ hits: [], ms: 0, embed_ms: 0, cache_hit: false }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  });
  vi.stubGlobal("fetch", fn);
  return calls;
}

const qs = (url: string) => new URL(url, "http://x").searchParams;

describe("search() URL builder", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("keeps the popup's positional call byte-identical", async () => {
    const calls = mockFetch();
    await search("hello", "hybrid", "canon");
    const p = qs(calls[0]);
    expect(p.get("q")).toBe("hello");
    expect(p.get("mode")).toBe("hybrid");
    expect(p.get("kb")).toBe("canon");
    for (const k of [
      "scope",
      "limit",
      "tags",
      "read",
      "sort",
      "dir",
      "detail",
      "caps",
      "since",
    ]) {
      expect(p.has(k)).toBe(false);
    }
  });

  it("serialises faceted axes as csv and drops defaults", async () => {
    const calls = mockFetch();
    await search("x", {
      mode: "hybrid",
      kb: "canon",
      scope: "one", // default → dropped
      tags: ["pm", "rust"],
      excludeTags: ["draft"],
      status: ["open"],
      caps: ["svg", "code"],
      since: "week",
      sinceField: "modified", // default → dropped
      read: ["unread", "in_progress"],
      sort: "relevance", // default → dropped
      rich: true,
    });
    const p = qs(calls[0]);
    expect(p.get("tags")).toBe("pm,rust");
    expect(p.get("exclude_tags")).toBe("draft");
    expect(p.get("status")).toBe("open");
    expect(p.get("caps")).toBe("svg,code");
    expect(p.get("since")).toBe("week");
    expect(p.get("read")).toBe("unread,in_progress");
    expect(p.get("detail")).toBe("full");
    expect(p.has("scope")).toBe(false);
    expect(p.has("since_field")).toBe(false);
    expect(p.has("sort")).toBe(false);
  });

  it("emits non-default scope, sort, dir, since_field, session, list", async () => {
    const calls = mockFetch();
    await search("x", {
      kb: "canon",
      scope: "all",
      sort: "opened",
      dir: "asc",
      sinceField: "created",
      session: "sess-1",
      list: "L1",
    });
    const p = qs(calls[0]);
    expect(p.get("scope")).toBe("all");
    expect(p.get("sort")).toBe("opened");
    expect(p.get("dir")).toBe("asc");
    expect(p.get("since_field")).toBe("created");
    expect(p.get("session")).toBe("sess-1");
    expect(p.get("list")).toBe("L1");
  });

  it("omits empty arrays", async () => {
    const calls = mockFetch();
    await search("x", { kb: "canon", tags: [], read: [], caps: [] });
    const p = qs(calls[0]);
    expect(p.has("tags")).toBe(false);
    expect(p.has("read")).toBe(false);
    expect(p.has("caps")).toBe(false);
  });
});
