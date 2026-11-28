// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DriveError } from "../phone/driveApi";
import {
  absPath,
  downloadFile,
  FILE_MAX_BYTES,
  listDir,
  pastePath,
  uploadFile,
  uploadMessage,
  type TransportRequest,
} from "./filesApi";

const ANSWER = { path: "/up/a.pdf", name: "a.pdf", bytes: 3, kind: "other", sha256: "s", mime: null, kb: { status: "skipped", reason: "not-configured", id: null, stub_rel: null, created: null, inlined: null } };
const file = (name = "a b&c.pdf", size = 3): File => new File([new Uint8Array(size)], name, { type: "application/pdf" });

describe("uploadFile", () => {
  it("encodes the name, adds index=1 only when set, and sends the intent header", async () => {
    const seen: TransportRequest[] = [];
    const transport = async (r: TransportRequest) => {
      seen.push(r);
      return { status: 200, statusText: "OK", text: JSON.stringify(ANSWER) };
    };
    await uploadFile("t1", file(), { transport });
    await uploadFile("t1", file("x.txt"), { transport, index: true });
    expect(seen[0].url).toBe("/api/terms/t1/files?name=a%20b%26c.pdf");
    expect(seen[1].url).toBe("/api/terms/t1/files?name=x.txt&index=1");
    expect(seen[0].headers["X-Kb-Intent"]).toBe("terminal");
  });

  it("refuses 32 MiB + 1 and empty files with no request", async () => {
    const transport = vi.fn();
    await expect(uploadFile("t1", { size: FILE_MAX_BYTES + 1 } as File, { transport })).rejects.toThrow(/32 MiB/);
    await expect(uploadFile("t1", file("e", 0), { transport })).rejects.toThrow(/empty/);
    expect(transport).not.toHaveBeenCalled();
  });

  it("maps the server's typed errors", async () => {
    const mk = (status: number, body: object) => async () => ({ status, statusText: "x", text: JSON.stringify(body) });
    const quota = await uploadFile("t1", file(), { transport: mk(413, { type: "urn:kb:errors:terminal-upload-quota", detail: "q" }) }).catch((e) => e);
    expect(uploadMessage(quota)).toMatch(/Upload area is full/);
    const big = await uploadFile("t1", file(), { transport: mk(413, { detail: "big" }) }).catch((e) => e);
    expect(uploadMessage(big)).toMatch(/32 MiB/);
    const down = await uploadFile("t1", file(), { transport: mk(503, { detail: "down" }) }).catch((e) => e);
    expect(uploadMessage(down)).toMatch(/runner/);
    const other = await uploadFile("t1", file(), { transport: mk(400, { detail: "bad name" }) }).catch((e) => e);
    expect(other).toBeInstanceOf(DriveError);
    expect(uploadMessage(other)).toBe("bad name");
  });
});

describe("paste", () => {
  let fetchMock: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    fetchMock = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
  });
  afterEach(() => vi.unstubAllGlobals());

  it("posts {text} only: no Enter, no enter key", async () => {
    await pastePath("t1", "/up/a.pdf");
    const [u, init] = fetchMock.mock.calls[0];
    expect(u).toBe("/api/terms/t1/paste");
    expect(JSON.parse(init.body as string)).toEqual({ text: "/up/a.pdf" });
    expect(init.body).not.toMatch(/\\[rn]/);
  });

  it("refuses a multi-line text with no request", async () => {
    await expect(pastePath("t1", "/up/a\nb")).rejects.toThrow(/line break/);
    await expect(pastePath("t1", "/up/a\rb")).rejects.toThrow();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("listDir + absPath", () => {
  afterEach(() => vi.unstubAllGlobals());
  it("sends root, an encoded path and the intent header", async () => {
    const f = vi.fn(async () => new Response(JSON.stringify({ root: "cwd", dir: "/w", entries: [], truncated: false, skipped: 0 }), { status: 200 }));
    vi.stubGlobal("fetch", f);
    await listDir("t1", "up", "a b/c");
    const [u, init] = f.mock.calls[0] as unknown as [string, RequestInit];
    expect(u).toBe("/api/terms/t1/ls?root=up&path=a%20b%2Fc");
    expect((init.headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
  });
  it("joins a directory and a name", () => {
    expect(absPath("/w", "a")).toBe("/w/a");
    expect(absPath("/w/", "a")).toBe("/w/a");
  });
});

describe("downloadFile", () => {
  const bodyOf = (bytes: number[], header: string | null): Response =>
    new Response(new Uint8Array(bytes), { status: 200, headers: header === null ? {} : { "Content-Length": header } });
  let create: ReturnType<typeof vi.fn>;
  let revoke: ReturnType<typeof vi.fn>;
  beforeEach(() => {
    create = vi.fn(() => "blob:x");
    revoke = vi.fn();
    Object.assign(URL, { createObjectURL: create, revokeObjectURL: revoke });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    delete (URL as unknown as Record<string, unknown>).createObjectURL;
    delete (URL as unknown as Record<string, unknown>).revokeObjectURL;
  });

  it("fetches with the intent header, clicks an anchor with download=name and revokes the URL", async () => {
    const f = vi.fn(async () => bodyOf([1, 2, 3], "3"));
    vi.stubGlobal("fetch", f);
    const clicks: string[] = [];
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      clicks.push(`${this.download}|${this.href}`);
    });
    const progress: number[] = [];
    await downloadFile("t1", "cwd", "d/out.txt", "out.txt", { onProgress: (l) => progress.push(l) });
    const [u, init] = f.mock.calls[0] as unknown as [string, RequestInit];
    expect(u).toBe("/api/terms/t1/file?root=cwd&path=d%2Fout.txt");
    expect((init.headers as Record<string, string>)["X-Kb-Intent"]).toBe("terminal");
    expect(clicks).toEqual(["out.txt|blob:x"]);
    expect(progress.at(-1)).toBe(3);
    expect(revoke).toHaveBeenCalledWith("blob:x");
    click.mockRestore();
  });

  it("a short body is an error, nothing is saved, and the URL is not leaked", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => bodyOf([1, 2], "5")));
    await expect(downloadFile("t1", "cwd", "a", "a")).rejects.toThrow(/ended early/);
    expect(create).not.toHaveBeenCalled();
  });

  it("revokes the URL even when the click throws", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => bodyOf([1], "1")));
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {
      throw new Error("blocked");
    });
    await expect(downloadFile("t1", "cwd", "a", "a")).rejects.toThrow("blocked");
    expect(revoke).toHaveBeenCalledWith("blob:x");
    click.mockRestore();
  });

  it("refuses over 32 MiB with no request, and surfaces an HTTP error", async () => {
    const f = vi.fn(async () => new Response(JSON.stringify({ detail: "gone" }), { status: 404 }));
    vi.stubGlobal("fetch", f);
    await expect(downloadFile("t1", "cwd", "a", "a", { bytes: FILE_MAX_BYTES + 1 })).rejects.toThrow(/32 MiB/);
    expect(f).not.toHaveBeenCalled();
    await expect(downloadFile("t1", "cwd", "a", "a")).rejects.toThrow("gone");
  });
});
