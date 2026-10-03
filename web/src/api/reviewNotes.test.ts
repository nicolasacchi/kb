import { describe, it, expect, vi, afterEach } from "vitest";
import { fetchReviewNotes, tagDelta } from "./reviewNotes";

// v0.40 TN — the note index's request URL is the wire contract the page's
// filter bar depends on: a dropped axis is a silently unfiltered list, and a
// default that rides the wire is a filter that looks applied and isn't. Two
// invariants: (1) every axis the operator set is ON the request, `tag`
// repeated once per value (ANDed server-side); (2) every default-out axis is
// ABSENT — no kb, no tags, no q and (the big one) no `status=all`. `bodies`
// is deliberately outside both: it is a shape switch, not a filter, so it
// rides the wire whenever the caller names it and never otherwise.

function mockFetch(): string[] {
  const calls: string[] = [];
  const fn = vi.fn(async (url: string | URL) => {
    calls.push(String(url));
    return new Response(
      JSON.stringify({ notes: [], tags: [], total: 0, truncated: false, tags_truncated: false }),
      { status: 200, headers: { "Content-Type": "application/json" } },
    );
  });
  vi.stubGlobal("fetch", fn);
  return calls;
}

const qs = (url: string) => new URL(url, "http://x").searchParams;

describe("fetchReviewNotes() URL builder", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends no params at all for the default view", async () => {
    const calls = mockFetch();
    await fetchReviewNotes();
    expect(qs(calls[0]).toString()).toBe("");
  });

  it("drops a kb/blank q/empty tag list that carry no filter", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({ kb: "", q: "   ", tags: [], status: "all" });
    expect(qs(calls[0]).toString()).toBe("");
  });

  it("repeats tag once per value (ANDed server-side, order preserved)", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({ tags: ["wording", "fleet-doc"] });
    expect(qs(calls[0]).getAll("tag")).toEqual(["wording", "fleet-doc"]);
  });

  it("carries kb, q and a non-default status", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({
      kb: "canon",
      q: "retry cap",
      tags: ["wording"],
      status: "open",
    });
    const p = qs(calls[0]);
    expect(p.get("kb")).toBe("canon");
    expect(p.get("q")).toBe("retry cap");
    expect(p.get("tag")).toBe("wording");
    expect(p.get("status")).toBe("open");
  });

  it("trims the q it sends (and drops it when it trims to nothing)", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({ q: "  cap  " });
    expect(qs(calls[0]).get("q")).toBe("cap");
    await fetchReviewNotes({ q: "   " });
    expect(qs(calls[1]).has("q")).toBe(false);
  });

  // `bodies` is the one axis that rides the wire by default-out: a caller
  // that never mentions it must get the server's own default, and one that
  // says `bodies=false` must actually get a body-free listing back.
  it("sends bodies=false when asked for, and nothing at all when not", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({ bodies: false });
    expect(qs(calls[0]).get("bodies")).toBe("false");
    await fetchReviewNotes();
    expect(qs(calls[1]).has("bodies")).toBe(false);
  });

  it("carries bodies alongside the other axes without displacing or merging into them", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({
      kb: "canon",
      q: "retry cap",
      tags: ["wording", "fleet-doc"],
      status: "open",
      bodies: false,
    });
    const p = qs(calls[0]);
    expect(p.get("bodies")).toBe("false");
    expect(p.get("kb")).toBe("canon");
    expect(p.get("q")).toBe("retry cap");
    expect(p.getAll("tag")).toEqual(["wording", "fleet-doc"]);
    expect(p.get("status")).toBe("open");
  });

  it("passes bodies=true through verbatim rather than default-dropping it", async () => {
    const calls = mockFetch();
    await fetchReviewNotes({ bodies: true });
    expect(qs(calls[0]).get("bodies")).toBe("true");
  });
});

// v0.44 X2 — the tag editor's wire form is a delta, never a full replace.
describe("tagDelta()", () => {
  it("adds what is new and removes what was dropped", () => {
    expect(tagDelta(["a", "b"], ["b", "c"])).toEqual({
      add_tags: ["c"],
      remove_tags: ["a"],
    });
  });

  it("an unchanged list is an empty add (a valid no-op), no remove key", () => {
    expect(tagDelta(["a", "b"], ["b", "a"])).toEqual({ add_tags: [] });
  });

  it("a case-only change is neither an add nor a remove", () => {
    expect(tagDelta(["wording"], ["Wording"])).toEqual({ add_tags: [] });
  });

  it("clearing every tag is removes only", () => {
    expect(tagDelta(["a"], [])).toEqual({ add_tags: [], remove_tags: ["a"] });
  });

  it("dedupes repeated additions", () => {
    expect(tagDelta([], ["x", "X", "x"])).toEqual({ add_tags: ["x"] });
  });
});
