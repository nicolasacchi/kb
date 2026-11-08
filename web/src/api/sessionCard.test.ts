import { afterEach, describe, expect, it, vi } from "vitest";
import { commitHref, fetchSessionCard, type SessionCardOut } from "./sessionCard";

// Node's fs without Node typings (same trick as lib/codeDoors.runtime.test.ts).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const fs = (globalThis as any).process.getBuiltinModule("node:fs") as {
  readFileSync(path: string, enc: "utf8"): string;
};
const here = (rel: string): string =>
  decodeURIComponent(new URL(rel, import.meta.url).pathname);

function mockFetch(body: unknown): string[] {
  const calls: string[] = [];
  const fn = vi.fn(async (url: string | URL) => {
    calls.push(String(url));
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fn);
  return calls;
}

afterEach(() => vi.unstubAllGlobals());

describe("sessionCard", () => {
  it("golden_fixture_parses_into_the_ts_type", () => {
    const text = fs.readFileSync(
      here("../../../tests/fixtures/session-card-v1.json"),
      "utf8",
    );
    const parsed = JSON.parse(text) as SessionCardOut;
    expect(parsed.schema).toBe("kbc-session-card/1");
    expect(parsed.sid).toBe("sess-0001");
    expect(parsed.commits.items).toHaveLength(3);
    expect(parsed.commits.items[0].confidence).toBe("trailer");
    expect(parsed.commits.items[2].visible).toBe(false);
    expect(parsed.reviews.items[0].basis).toBe("session_id");
    expect(parsed.findings.by_severity).toEqual({ blocker: 1, concern: 2, ok: 1 });
    expect(parsed.presence).toEqual({ available: true, files: 2, newest_age_secs: 40 });
  });

  it("fetch_uses_credentials_include_and_trims_the_base", async () => {
    const calls = mockFetch({ schema: "kbc-session-card/1" });
    const fn = vi.mocked(fetch);
    await fetchSessionCard("https://code.example.test///", "sess-0001");
    expect(calls).toHaveLength(1);
    expect(calls[0]).toBe(
      "https://code.example.test/api/session-card?sid=sess-0001",
    );
    // `getCrossOrigin` sends credentials include (the credentialed read).
    const init = fn.mock.calls[0][1] as RequestInit | undefined;
    expect(init?.credentials).toBe("include");
  });

  it("fetch_carries_the_repo_filter", async () => {
    const calls = mockFetch({});
    await fetchSessionCard("https://code.example.test", "sess-0001", "fixture");
    expect(calls[0]).toContain("sid=sess-0001");
    expect(calls[0]).toContain("repo=fixture");
  });

  it("commit_href_uses_the_real_kb_code_route", () => {
    expect(
      commitHref("https://code.example.test/", "fixture", "abc123"),
    ).toBe("https://code.example.test/r/fixture/~commit/abc123");
    expect(commitHref("https://code.example.test", "we ird", "a/b")).toBe(
      "https://code.example.test/r/we%20ird/~commit/a%2Fb",
    );
  });
});
