import { describe, it, expect, vi, afterEach } from "vitest";
import {
  deriveMemoryTitle,
  pickMemoryKb,
  rememberMemory,
  type KbSummary,
} from "./client";

// U3 — highlight → save as memory. Three contracts pinned here:
//   1. the memory write rides the EXISTING ingest route + body (no new
//      endpoint, no new store) and carries the provenance fields verbatim,
//   2. the target corpus is resolved exactly the way `kb remember` resolves
//      it (single project corpus, else single global; ambiguity ⇒ refuse),
//   3. the title is DERIVED by a deterministic string rule — never a
//      generated summary (kb runs no model on either side of the wire).

function kb(name: string, memory_scope: string | null): KbSummary {
  return {
    name,
    path: `/srv/${name}`,
    doc_count: 0,
    last_index_at: null,
    memory_scope,
    default_search_category: null,
    code_url: null,
  };
}

function mockFetch(): { url: string; body: unknown }[] {
  const calls: { url: string; body: unknown }[] = [];
  const fn = vi.fn(async (url: string | URL, init?: RequestInit) => {
    calls.push({
      url: String(url),
      body: init?.body ? JSON.parse(String(init.body)) : undefined,
    });
    return new Response(JSON.stringify({ id: "abc123abc123", path: "m.html" }), {
      status: 201,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fn);
  return calls;
}

describe("pickMemoryKb", () => {
  it("prefers the single project memory corpus", () => {
    const kbs = [kb("docs", null), kb("mem", "project"), kb("global", "global")];
    expect(pickMemoryKb(kbs)).toBe("mem");
  });

  it("falls back to the single global corpus", () => {
    expect(pickMemoryKb([kb("docs", null), kb("g", "global")])).toBe("g");
  });

  it("refuses to guess when the choice is ambiguous or absent", () => {
    // Two project corpora — the CLI errors here; the SPA disables the
    // action rather than writing a memory into the wrong corpus.
    expect(pickMemoryKb([kb("a", "project"), kb("b", "project")])).toBeNull();
    expect(pickMemoryKb([kb("docs", null)])).toBeNull();
    expect(pickMemoryKb([])).toBeNull();
    expect(pickMemoryKb(undefined)).toBeNull();
  });
});

describe("deriveMemoryTitle", () => {
  it("takes the first non-empty line, trimmed", () => {
    expect(deriveMemoryTitle("\n  hello world  \nmore")).toBe("hello world");
  });

  it("caps at 80 characters like the CLI's derive_title", () => {
    expect(deriveMemoryTitle("x".repeat(200))).toHaveLength(80);
  });

  it("falls back to 'memory' when nothing survives", () => {
    expect(deriveMemoryTitle("   \n  ")).toBe("memory");
    expect(deriveMemoryTitle("")).toBe("memory");
  });
});

describe("rememberMemory (the ONE memory write path)", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("POSTs the existing ingest route with the provenance fields", async () => {
    const calls = mockFetch();
    await rememberMemory("mem", {
      title: "a claim",
      body: "the selection, verbatim",
      author: "you",
      source_kb: "kb-docs",
      source_artifact: "a1b2c3d4e5f6",
      source_anchor: {
        kind: "selection",
        css_path: "main > p:nth-of-type(2)",
        offset: 17,
        snippet: "the selection, verbatim",
      },
    });
    expect(calls).toHaveLength(1);
    // The SAME route `kb remember` drives — not a new endpoint.
    expect(calls[0].url).toBe("/api/kb/mem/artifacts");
    expect(calls[0].body).toEqual({
      title: "a claim",
      body: "the selection, verbatim",
      author: "you",
      source_kb: "kb-docs",
      source_artifact: "a1b2c3d4e5f6",
      source_anchor: {
        kind: "selection",
        css_path: "main > p:nth-of-type(2)",
        offset: 17,
        snippet: "the selection, verbatim",
      },
    });
  });

  it("sends no salience/decay thumb on the scale", async () => {
    const calls = mockFetch();
    await rememberMemory("mem", { title: "t", body: "b", author: "you" });
    const body = calls[0].body as Record<string, unknown>;
    // Ruling 5: a "human memories are more important" multiplier is a
    // score term by another name. The server default stands.
    expect(body).not.toHaveProperty("salience");
    expect(body).not.toHaveProperty("decay");
    expect(body).not.toHaveProperty("pinned");
  });
});
