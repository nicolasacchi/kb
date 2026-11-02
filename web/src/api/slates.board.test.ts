import { afterEach, describe, expect, it, vi } from "vitest";
import { fetchSlateBoard } from "./slates";

// SLATE2 — the board folds aged found/idea posts server-side; `all: true`
// is the only way the SPA asks for them.

function mockFetch() {
  const urls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL) => {
      urls.push(String(url));
      return new Response(
        JSON.stringify({
          slug: "orchard",
          head_seq: 0,
          topics: [],
          sections: {},
          folded_found_idea: 3,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }),
  );
  return urls;
}

describe("fetchSlateBoard()", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("does not ask for aged posts by default", async () => {
    const urls = mockFetch();
    const board = await fetchSlateBoard("orchard");
    expect(urls[0]).toBe("/api/slates/orchard?view=board");
    expect(board.folded_found_idea).toBe(3);
  });

  it("sends all=1 only when asked, alongside the topic filter", async () => {
    const urls = mockFetch();
    await fetchSlateBoard("orchard", { topic: "graft", all: true });
    const q = new URL(urls[0], "http://x").searchParams;
    expect(q.get("view")).toBe("board");
    expect(q.get("topic")).toBe("graft");
    expect(q.get("all")).toBe("1");
  });
});
