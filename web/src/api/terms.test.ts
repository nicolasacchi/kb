import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  listTerms,
  mintTicket,
  TERMINAL_INTENT_HEADER,
  TERMINAL_INTENT_VALUE,
  TermsApiError,
  wsUrl,
} from "./terms";
import { setDaemonBase } from "./base";

interface Call {
  url: string;
  init?: RequestInit;
}

function mockFetch(handler: (url: string) => Response | Promise<Response>) {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string | URL, init?: RequestInit) => {
      calls.push({ url: String(url), init });
      return handler(String(url));
    }),
  );
  return calls;
}

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function header(init: RequestInit | undefined, name: string): string | undefined {
  const h = init?.headers as Record<string, string> | undefined;
  return h?.[name];
}

describe("terms API", () => {
  afterEach(() => vi.unstubAllGlobals());
  beforeEach(() => {
    vi.stubGlobal("location", { protocol: "http:", host: "127.0.0.1:4737" });
  });

  it("pins the intent-header pair as literals (crates/kb-server/src/terminal/gate.rs)", () => {
    expect(TERMINAL_INTENT_HEADER).toBe("X-Kb-Intent");
    expect(TERMINAL_INTENT_VALUE).toBe("terminal");
  });

  it("mintTicket POSTs with the intent header and no Authorization/X-Kb-Token", async () => {
    const calls = mockFetch(() =>
      jsonResponse(200, { ticket: "a".repeat(32), expires_in: 30, term: "abc123" }),
    );
    const ticket = await mintTicket("abc123");
    expect(ticket.ticket).toBe("a".repeat(32));
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/terms/abc123/ticket");
    const init = calls[0].init!;
    expect(init.method).toBe("POST");
    expect(header(init, "X-Kb-Intent")).toBe("terminal");
    expect(header(init, "Accept")).toBe("application/json");
    expect(header(init, "Authorization")).toBeUndefined();
    expect(header(init, "X-Kb-Token")).toBeUndefined();
    expect(header(init, "Origin")).toBeUndefined();
    expect(init.body).toBe("{}");
  });

  it("listTerms GETs /api/terms and unwraps the {terms} envelope", async () => {
    const calls = mockFetch(() => jsonResponse(200, { terms: [{ id: "t1" }] }));
    const rows = await listTerms();
    expect(rows).toEqual([{ id: "t1" }]);
    expect(calls[0].url).toBe("/api/terms");
    expect(calls[0].init?.method).toBeUndefined();
  });

  it("wsUrl uses ws: for an http daemon, wss: for https", () => {
    expect(wsUrl("t1", "tick")).toBe("ws://127.0.0.1:4737/api/terms/t1/ws?ticket=tick");
    vi.stubGlobal("location", { protocol: "https:", host: "kb.example.com" });
    expect(wsUrl("t1", "tick")).toBe("wss://kb.example.com/api/terms/t1/ws?ticket=tick");
  });

  it("wsUrl derives scheme and host from a non-empty daemon base", () => {
    setDaemonBase("https://kb.example.com:4000");
    try {
      expect(wsUrl("t1", "tick", 5)).toBe("wss://kb.example.com:4000/api/terms/t1/ws?ticket=tick&from=5");
    } finally {
      setDaemonBase("");
    }
    // http base → ws.
    setDaemonBase("http://laptop.local:4000");
    try {
      expect(wsUrl("t1", "tick")).toBe("ws://laptop.local:4000/api/terms/t1/ws?ticket=tick");
    } finally {
      setDaemonBase("");
    }
  });

  it("404 on mintTicket throws TermsApiError with status 404", async () => {
    mockFetch(() => new Response("", { status: 404 }));
    const err = await mintTicket("gone").catch((e) => e);
    expect(err).toBeInstanceOf(TermsApiError);
    expect((err as TermsApiError).status).toBe(404);
  });

  it("400 problem+json parses type into TermsApiError.type", async () => {
    mockFetch(() =>
      jsonResponse(400, {
        type: "urn:kb:errors:bad-request",
        title: "Bad Request",
        detail: "invalid terminal id",
      }),
    );
    const err = await mintTicket("bad/id").catch((e) => e);
    expect(err).toBeInstanceOf(TermsApiError);
    expect((err as TermsApiError).status).toBe(400);
    expect((err as TermsApiError).type).toBe("urn:kb:errors:bad-request");
  });
  it("listTerms 500 throws TermsApiError with status 500", async () => {
    mockFetch(() => new Response("", { status: 500 }));
    const err = await listTerms().catch((e) => e);
    expect(err).toBeInstanceOf(TermsApiError);
    expect((err as TermsApiError).status).toBe(500);
  });

  it("mintTicket URL-encodes the term id", async () => {
    const calls = mockFetch(() =>
      jsonResponse(200, { ticket: "a".repeat(32), expires_in: 30, term: "x" }),
    );
    await mintTicket("a/b c?d");
    expect(calls[0].url).toBe(`/api/terms/${encodeURIComponent("a/b c?d")}/ticket`);
  });
});
