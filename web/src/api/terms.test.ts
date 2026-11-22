import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  killTerm,
  launchTerm,
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

  it("launchTerm POSTs the JSON body with the intent header and returns the runner confirm", async () => {
    const calls = mockFetch(() => jsonResponse(201, { id: "t9", cols: 120, rows: 40 }));
    const out = await launchTerm({ harness: "claude", cwd: "/work/p", handoff_from: "sid-1" });
    expect(out).toEqual({ id: "t9", cols: 120, rows: 40 });
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/terms");
    const init = calls[0].init!;
    expect(init.method).toBe("POST");
    expect(header(init, "X-Kb-Intent")).toBe("terminal");
    expect(header(init, "Accept")).toBe("application/json");
    expect(header(init, "Content-Type")).toBe("application/json");
    expect(header(init, "Authorization")).toBeUndefined();
    expect(header(init, "X-Kb-Token")).toBeUndefined();
    expect(JSON.parse(String(init.body))).toEqual({
      harness: "claude",
      cwd: "/work/p",
      handoff_from: "sid-1",
    });
  });

  it("launchTerm throws TermsApiError with the problem reason on non-2xx", async () => {
    mockFetch(() =>
      jsonResponse(400, {
        type: "urn:kb:errors:bad-request",
        title: "Bad Request",
        detail: "invalid handoff_from",
      }),
    );
    await expect(
      launchTerm({ harness: "claude", cwd: "/work/p", handoff_from: "bad id" }),
    ).rejects.toMatchObject({
      name: "TermsApiError",
      status: 400,
      type: "urn:kb:errors:bad-request",
      message: "Bad Request: invalid handoff_from",
    });
  });

  it("listTerms GETs /api/terms and unwraps the {terms} envelope", async () => {
    const calls = mockFetch(() => jsonResponse(200, { terms: [{ id: "t1" }] }));
    const rows = await listTerms();
    expect(rows).toEqual([{ id: "t1" }]);
    expect(calls[0].url).toBe("/api/terms");
    expect(calls[0].init?.method).toBeUndefined();
  });

  it("listTerms sends the intent header (a same-origin GET has no Origin, so intent is what the GET gate admits)", async () => {
    const calls = mockFetch(() => jsonResponse(200, { terms: [] }));
    await listTerms();
    const headers = calls[0].init?.headers as Record<string, string>;
    expect(headers[TERMINAL_INTENT_HEADER]).toBe(TERMINAL_INTENT_VALUE);
    // No Origin and no credentials header are ever set by the SPA.
    expect(Object.keys(headers).map((k) => k.toLowerCase())).not.toContain("origin");
  });

  it("wsUrl uses ws: for an http daemon, wss: for https", () => {
    expect(wsUrl("t1", "tick")).toBe("ws://127.0.0.1:4737/api/terms/t1/ws?ticket=tick");
    vi.stubGlobal("location", { protocol: "https:", host: "kb.example.com" });
    expect(wsUrl("t1", "tick")).toBe("wss://kb.example.com/api/terms/t1/ws?ticket=tick");
  });

  it("wsUrl carries epoch and mseq only next to a resume offset", () => {
    setDaemonBase("http://laptop.local:4000");
    try {
      const base = "ws://laptop.local:4000/api/terms/t1/ws?ticket=tick";
      expect(wsUrl("t1", "tick", 5, "e1")).toBe(`${base}&from=5&epoch=e1`);
      expect(wsUrl("t1", "tick", 5, "e1", 7)).toBe(`${base}&from=5&epoch=e1&mseq=7`);
      expect(wsUrl("t1", "tick", 5, "e1", 0)).toBe(`${base}&from=5&epoch=e1`);
      // No offset: a full join, so the epoch and mseq are dropped.
      expect(wsUrl("t1", "tick", undefined, "e1", 7)).toBe(base);
      expect(wsUrl("t1", "tick", 0, "e1", 7)).toBe(base);
    } finally {
      setDaemonBase("");
    }
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

  it("mintTicket never follows a redirect: an edge-auth login redirect is an auth-expired error", async () => {
    const calls = mockFetch(() => ({ type: "opaqueredirect", ok: false, status: 0, statusText: "" }) as unknown as Response);
    const err = await mintTicket("abc123").catch((e) => e);
    expect(calls[0].init?.redirect).toBe("manual");
    expect(err).toBeInstanceOf(TermsApiError);
    expect((err as TermsApiError).status).toBe(401);
    expect((err as TermsApiError).type).toBe("urn:kb:errors:auth-expired");
  });

  it("mintTicket keeps a 401 as status 401", async () => {
    mockFetch(() => new Response("<html>login</html>", { status: 401 }));
    const err = await mintTicket("abc123").catch((e) => e);
    expect(err).toBeInstanceOf(TermsApiError);
    expect((err as TermsApiError).status).toBe(401);
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

  it("killTerm POSTs /api/terms/{id}/kill with the intent header, no body, no token (FUTERMLIST)", async () => {
    const calls = mockFetch(() => jsonResponse(200, { exit_code: null }));
    await expect(killTerm("abc123")).resolves.toBeUndefined();
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/terms/abc123/kill");
    expect(calls[0].init?.method).toBe("POST");
    expect(header(calls[0].init, TERMINAL_INTENT_HEADER)).toBe(TERMINAL_INTENT_VALUE);
    expect(header(calls[0].init, "Authorization")).toBeUndefined();
    expect(header(calls[0].init, "X-Kb-Token")).toBeUndefined();
    expect(calls[0].init?.body ?? null).toBeNull();
  });

  it("killTerm encodes the id and throws TermsApiError on a non-2xx", async () => {
    const calls = mockFetch(() =>
      jsonResponse(404, { type: "urn:kb:errors:terminal-not-found", title: "Not Found", detail: "gone" }),
    );
    await expect(killTerm("a/b")).rejects.toMatchObject({ name: "TermsApiError", status: 404 });
    expect(calls[0].url).toBe("/api/terms/a%2Fb/kill");
  });
});
