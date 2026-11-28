// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DriveError } from "../phone/driveApi";
import { FILE_CAP_MESSAGE, FILE_MAX_BYTES } from "./filesApi";
import { keepAnswerProblem, keepFile, keepMessage, keepRefusal } from "./keepApi";

const ANSWER = { name: "a.txt", bytes: 3, sha256: "s", kb: { status: "stored", reason: null, id: "i1", stub_rel: "files/a.html", created: true, inlined: false } };
let f: ReturnType<typeof vi.fn>;
beforeEach(() => {
  f = vi.fn(async () => new Response(JSON.stringify(ANSWER), { status: 200 }));
  vi.stubGlobal("fetch", f);
});
afterEach(() => vi.unstubAllGlobals());

describe("keepFile", () => {
  it("posts {path, root, index:false} with the intent header by default", async () => {
    const a = await keepFile("t1", { root: "cwd", path: "d/a b.txt" });
    expect(a.kb.id).toBe("i1");
    const [u, init] = f.mock.calls[0] as unknown as [string, RequestInit];
    expect(u).toBe("/api/terms/t1/files/keep");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ path: "d/a b.txt", root: "cwd", index: false });
    const h = init.headers as Record<string, string>;
    expect(h["X-Kb-Intent"]).toBe("terminal");
    expect(h["Content-Type"]).toBe("application/json");
    expect(h.Accept).toBe("application/json");
  });

  it("sends index:true only when asked, and the up root", async () => {
    await keepFile("t1", { root: "up", path: "a.txt", index: true });
    const init = f.mock.calls[0][1] as RequestInit;
    expect(JSON.parse(init.body as string)).toEqual({ path: "a.txt", root: "up", index: true });
  });

  it("throws a typed DriveError for a problem answer", async () => {
    f.mockResolvedValueOnce(new Response(JSON.stringify({ type: "urn:kb:errors:terminal-file-changed", detail: "x" }), { status: 409 }));
    const e = await keepFile("t1", { root: "cwd", path: "a" }).catch((x) => x);
    expect(e).toBeInstanceOf(DriveError);
    expect(e.type).toBe("urn:kb:errors:terminal-file-changed");
  });
});

describe("keepMessage", () => {
  const err = (status: number, type: string | null, detail = "server words") => new DriveError(status, type, detail);
  it("maps every typed refusal", () => {
    expect(keepMessage(err(409, "urn:kb:errors:terminal-files-not-configured"))).toBe("Keeping files in kb is not configured on this server.");
    expect(keepMessage(err(409, "urn:kb:errors:terminal-file-changed"))).toBe("The file changed while it was being read. Try again.");
    expect(keepMessage(err(413, "urn:kb:errors:terminal-upload-quota"))).toMatch(/quota/);
    expect(keepMessage(err(413, "urn:kb:errors:payload-too-large"))).toBe(FILE_CAP_MESSAGE);
    expect(keepMessage(err(503, null))).toMatch(/runner is not reachable/);
  });
  it("shows the server's own detail for 400, 403, 404, 422 and 500", () => {
    for (const s of [400, 403, 404, 422, 500]) expect(keepMessage(err(s, null))).toBe("server words");
    expect(keepMessage(new Error("x"))).toBe("Could not keep the file in kb.");
  });
});

describe("keepRefusal / keepAnswerProblem", () => {
  it("refuses over 32 MiB before any request", () => {
    expect(keepRefusal({ bytes: FILE_MAX_BYTES })).toBeNull();
    expect(keepRefusal({ bytes: FILE_MAX_BYTES + 1 })).toBe(FILE_CAP_MESSAGE);
  });
  it("names skipped and failed answers, and is silent for stored", () => {
    const a = (status: string, reason: string | null) => ({ ...ANSWER, kb: { ...ANSWER.kb, status, reason } });
    expect(keepAnswerProblem(a("stored", null))).toBeNull();
    expect(keepAnswerProblem(a("skipped", "quota"))).toBe("Not kept in kb: quota");
    expect(keepAnswerProblem(a("failed", "disk"))).toBe("kb keep failed: disk");
  });
});
