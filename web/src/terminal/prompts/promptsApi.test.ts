import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { addPrompt, deletePrompt, fetchPrompts, patchPrompt, PromptsApiError, PROMPTS_STALE_TYPE, termPromptsKey } from "./promptsApi";

interface Call {
  url: string;
  init: RequestInit;
}
let calls: Call[] = [];
let reply: () => Response;
const json = (status: number, body: unknown, type = "application/json") => new Response(JSON.stringify(body), { status, headers: { "Content-Type": type } });
const LIST = { project: "demo", rev: 2, items: [] };

beforeEach(() => {
  calls = [];
  reply = () => json(200, LIST);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return reply();
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

const hdr = (c: Call) => c.init.headers as Record<string, string>;
const SECRET = "secret prompt text with spaces & ? # %";

describe("prompts API client", () => {
  it("keys the query by project", () => {
    expect(termPromptsKey("demo")).toEqual(["term", "prompts", "demo"]);
  });

  it("sends the terminal intent header and no token on every call", async () => {
    await fetchPrompts("demo");
    await addPrompt("demo", "x");
    await patchPrompt("demo", "q_0123456789ab", { done: true }, 2);
    await deletePrompt("demo", "q_0123456789ab", 2);
    expect(calls).toHaveLength(4);
    for (const c of calls) {
      expect(hdr(c)["X-Kb-Intent"]).toBe("terminal");
      expect(JSON.stringify(hdr(c)).toLowerCase()).not.toContain("authorization");
      expect(JSON.stringify(hdr(c)).toLowerCase()).not.toContain("x-kb-token");
    }
  });

  it("encodes the project and item, and quotes If-Match", async () => {
    await patchPrompt("a_b-1", "q_0123456789ab", { position: 0 }, 7);
    const c = calls[0];
    expect(c.url).toMatch(/\/api\/term-prompts\/a_b-1\/q_0123456789ab$/);
    expect(c.init.method).toBe("PATCH");
    expect(hdr(c)["If-Match"]).toBe('"7"');
    await deletePrompt("we/ird", "q_0123456789ab", 3);
    expect(calls[1].url).toContain("/api/term-prompts/we%2Fird/");
    expect(hdr(calls[1])["If-Match"]).toBe('"3"');
  });

  it("puts prompt text only in the JSON body, never in the URL", async () => {
    await addPrompt("demo", SECRET);
    await patchPrompt("demo", "q_0123456789ab", { text: SECRET }, 1);
    for (const c of calls) {
      expect(c.url).not.toContain("secret");
      expect(c.url).not.toContain(encodeURIComponent(SECRET));
      expect(JSON.parse(String(c.init.body)).text).toBe(SECRET);
    }
  });

  it("turns a 412 into a typed error carrying the current rev", async () => {
    reply = () => json(412, { type: PROMPTS_STALE_TYPE, title: "Precondition Failed", detail: "changed", rev: 9, status: 412 }, "application/problem+json");
    const err = await patchPrompt("demo", "q_0123456789ab", { done: true }, 1).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(PromptsApiError);
    const e = err as PromptsApiError;
    expect(e.stale).toBe(true);
    expect(e.rev).toBe(9);
    expect(e.type).toBe(PROMPTS_STALE_TYPE);
  });

  it("carries the problem detail for a 400 / 409 and tolerates a bodiless failure", async () => {
    reply = () => json(409, { type: "urn:kb:errors:queue-full", title: "Conflict", detail: "this project's prompt queue is full" });
    const e = (await addPrompt("demo", "x").catch((x: unknown) => x)) as PromptsApiError;
    expect(e.status).toBe(409);
    expect(e.detail).toContain("full");
    expect(e.rev).toBeNull();
    reply = () => new Response(null, { status: 404 });
    const n = (await fetchPrompts("demo").catch((x: unknown) => x)) as PromptsApiError;
    expect(n.status).toBe(404);
  });
});
