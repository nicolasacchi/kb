// HUBHOST — fetchSessionStrict: only a 404 is "unknown"; every other failure
// throws. The shared lenient fetchSession keeps returning null on a 500.
import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionFetchError, fetchSession, fetchSessionStrict } from "./sessions";

function stub(status: number, body: unknown = {}) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(JSON.stringify(body), { status })),
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("fetchSessionStrict", () => {
  it("200 returns the row", async () => {
    stub(200, { session_id: "s1" });
    expect(await fetchSessionStrict("s1")).toEqual({ session_id: "s1" });
  });
  it("404 returns null", async () => {
    stub(404);
    expect(await fetchSessionStrict("s1")).toBeNull();
  });
  it("500 throws SessionFetchError carrying the status", async () => {
    stub(500);
    const e = await fetchSessionStrict("s1").catch((x: unknown) => x);
    expect(e).toBeInstanceOf(SessionFetchError);
    expect((e as SessionFetchError).status).toBe(500);
  });
  it("a network failure rejects", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new Error("down"))));
    await expect(fetchSessionStrict("s1")).rejects.toThrow("down");
  });
  it("the shared fetchSession still answers null on a 500", async () => {
    vi.spyOn(console, "warn").mockImplementation(() => {});
    stub(500);
    expect(await fetchSession("s1")).toBeNull();
  });
});
