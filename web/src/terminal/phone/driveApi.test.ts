// T18 — the Drive REST client: every POST carries the intent header and the
// viewer, the Claude guard runs before any request, and an upload is refused
// client-side before it is fetched.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  DriveError,
  paste,
  pressEnter,
  releaseSize,
  sendKey,
  sendText,
  takeSize,
  type Drive,
} from "./driveApi";

interface Call {
  url: string;
  init: RequestInit;
}

let calls: Call[] = [];
let replies: Array<Response | (() => Response)> = [];

function json(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

beforeEach(() => {
  calls = [];
  replies = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      const next = replies.shift();
      if (next === undefined) return json(200, {});
      return typeof next === "function" ? next() : next;
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
});

const mkDrive = (over: Partial<Drive> = {}): Drive => ({
  termId: "abcd1234",
  harness: "omp",
  viewer: "ws-abc1234567",
  confirm: vi.fn(async () => true),
  ...over,
});

const headers = (c: Call): Record<string, string> => c.init.headers as Record<string, string>;
const body = (c: Call): Record<string, unknown> => JSON.parse(String(c.init.body)) as Record<string, unknown>;

describe("every JSON POST", () => {
  it("carries the terminal intent header, JSON content type and the viewer", async () => {
    const d = mkDrive();
    await paste(d, "hi");
    await pressEnter(d);
    await sendKey(d, "Tab");
    await takeSize(d, 50, 20);
    expect(calls.map((c) => c.url)).toEqual([
      "/api/terms/abcd1234/paste",
      "/api/terms/abcd1234/enter",
      "/api/terms/abcd1234/keys",
      "/api/terms/abcd1234/take-size",
    ]);
    for (const c of calls) {
      expect(c.init.method).toBe("POST");
      expect(headers(c)["X-Kb-Intent"]).toBe("terminal");
      expect(headers(c)["Content-Type"]).toBe("application/json");
      expect(body(c).viewer).toBe("ws-abc1234567");
      // Same-origin only: no token, and never a hand-set Origin.
      expect(headers(c).Authorization).toBeUndefined();
      expect(headers(c).Origin).toBeUndefined();
    }
    expect(body(calls[0])).toEqual({ text: "hi", viewer: "ws-abc1234567" });
    expect(body(calls[2])).toEqual({ keys: ["Tab"], viewer: "ws-abc1234567" });
    expect(body(calls[3])).toEqual({ cols: 50, rows: 20, viewer: "ws-abc1234567" });
  });

  it("release-size posts exactly `{}` (the route rejects unknown fields, so no viewer) with the intent header", async () => {
    await releaseSize(mkDrive());
    expect(calls.map((c) => c.url)).toEqual(["/api/terms/abcd1234/release-size"]);
    expect(calls[0].init.method).toBe("POST");
    expect(body(calls[0])).toEqual({});
    expect(headers(calls[0])["X-Kb-Intent"]).toBe("terminal");
  });

  it("omits the viewer when the tap named none (an older tap)", async () => {
    await paste(mkDrive({ viewer: null }), "hi");
    expect(body(calls[0])).toEqual({ text: "hi" });
  });

  it("turns a problem+json answer into a DriveError carrying the detail verbatim", async () => {
    replies.push(
      json(409, { type: "urn:kb:errors:terminal-conflict", title: "Conflict", detail: "grid is owned by desk-1" }),
    );
    const err = await takeSize(mkDrive(), 50, 20).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(DriveError);
    expect((err as DriveError).status).toBe(409);
    expect((err as DriveError).type).toBe("urn:kb:errors:terminal-conflict");
    expect((err as DriveError).detail).toBe("grid is owned by desk-1");
  });

  it("falls back to the status line for a bodiless answer (terminal disabled)", async () => {
    replies.push(new Response(null, { status: 404, statusText: "Not Found" }));
    const err = (await paste(mkDrive(), "x").catch((e: unknown) => e)) as DriveError;
    expect(err.status).toBe(404);
    expect(err.detail).toBe("404 Not Found");
  });
});

describe("sendKey and sendText: the one guard", () => {
  it("asks before ANY request for claude Left, and declining sends nothing", async () => {
    const confirm = vi.fn(async () => false);
    const d = mkDrive({ harness: "claude", confirm });
    const r = await sendKey(d, "Left");
    expect(r).toEqual({ kind: "declined" });
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(calls).toHaveLength(0);
  });

  it("sends the key once the operator confirms", async () => {
    const confirm = vi.fn(async () => true);
    const r = await sendKey(mkDrive({ harness: "claude", confirm }), "Left");
    expect(r).toEqual({ kind: "sent" });
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(calls).toHaveLength(1);
  });

  it("never asks for another key or another harness", async () => {
    const confirm = vi.fn(async () => true);
    await sendKey(mkDrive({ harness: "claude", confirm }), "Right");
    await sendKey(mkDrive({ harness: "omp", confirm }), "Left");
    expect(confirm).not.toHaveBeenCalled();
    expect(calls).toHaveLength(2);
  });

  it("asks before pasting exactly `/bg` on claude; declining writes nothing", async () => {
    const confirm = vi.fn(async () => false);
    const r = await sendText(mkDrive({ harness: "claude", confirm }), "/bg", { enter: false });
    expect(r).toEqual({ kind: "declined" });
    expect(calls).toHaveLength(0);
  });

  it("without enter, only pastes", async () => {
    await sendText(mkDrive(), "/compact", { enter: false });
    expect(calls.map((c) => c.url)).toEqual(["/api/terms/abcd1234/paste"]);
  });

  it("a failed paste throws (nothing was written)", async () => {
    replies.push(json(422, { type: "urn:kb:errors:terminal-refused", detail: "multi-line paste refused" }));
    await expect(sendText(mkDrive(), "a\nb", { enter: true })).rejects.toBeInstanceOf(DriveError);
    expect(calls).toHaveLength(1);
  });
});

describe("sendText with enter is ONE round trip (FUPHONESEND)", () => {
  it("posts a single paste carrying enter:true and no separate enter request", async () => {
    replies.push(json(200, { paste_only: false, bracketed: true, entered: true }));
    const r = await sendText(mkDrive(), "hello", { enter: true });
    expect(r).toEqual({ kind: "sent" });
    expect(calls.map((c) => c.url)).toEqual(["/api/terms/abcd1234/paste"]);
    expect(body(calls[0])).toEqual({ text: "hello", enter: true, viewer: "ws-abc1234567" });
  });

  it("without enter the body carries no enter key", async () => {
    await sendText(mkDrive(), "/compact", { enter: false });
    expect(body(calls[0])).toEqual({ text: "/compact", viewer: "ws-abc1234567" });
  });

  it("entered:false in a 200 is pasted-not-sent, conflict when the refusal was 409", async () => {
    replies.push(
      json(200, {
        paste_only: false,
        bracketed: true,
        entered: false,
        enter_error: { status: 409, type: "urn:kb:errors:term-blocked", detail: "the bound session is blocked" },
      }),
    );
    const r = await sendText(mkDrive(), "hello", { enter: true });
    expect(r).toEqual({ kind: "pasted-not-sent", detail: "the bound session is blocked", conflict: true });
    replies.push(
      json(200, { paste_only: false, bracketed: true, entered: false, enter_error: { status: 503, type: "x", detail: "runner down" } }),
    );
    const r2 = await sendText(mkDrive(), "hello", { enter: true });
    expect(r2).toEqual({ kind: "pasted-not-sent", detail: "runner down", conflict: false });
  });

  it("a failed combined request throws (nothing was written)", async () => {
    replies.push(json(422, { type: "urn:kb:errors:terminal-refused", detail: "multi-line paste refused" }));
    await expect(sendText(mkDrive(), "a\nb", { enter: true })).rejects.toBeInstanceOf(DriveError);
    expect(calls).toHaveLength(1);
  });
});
