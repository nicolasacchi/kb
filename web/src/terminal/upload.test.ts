// FUTERMPASTE — the shared picture upload: the type and size gate, the raw
// POST, and reading picture files out of a paste or a drop.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DriveError } from "./phone/driveApi";
import {
  UPLOAD_MAX_BYTES,
  droppedFiles,
  dragHasFiles,
  pastedPictureFiles,
  uploadImage,
  uploadRefusal,
} from "./upload";
import { fakeDt } from "./upload.testkit";

interface Call {
  url: string;
  init: RequestInit;
}
let calls: Call[] = [];
let replies: Response[] = [];
const json = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
const headers = (c: Call): Record<string, string> => c.init.headers as Record<string, string>;

beforeEach(() => {
  calls = [];
  replies = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init: RequestInit) => {
      calls.push({ url, init });
      return replies.shift() ?? json(200, {});
    }),
  );
});
afterEach(() => vi.unstubAllGlobals());

describe("uploadImage", () => {
  const file = (size: number, type: string): File => new File([new Uint8Array(size)], "x", { type });

  it("refuses a wrong type or an oversize file before fetching", async () => {
    for (const bad of [file(10, "image/svg+xml"), file(10, "text/html"), file(10, "application/pdf"), file(10, "")]) {
      await expect(uploadImage("abcd1234", bad)).rejects.toBeInstanceOf(DriveError);
    }
    await expect(uploadImage("abcd1234", file(UPLOAD_MAX_BYTES + 1, "image/png"))).rejects.toThrow(/8 MiB/);
    expect(calls).toHaveLength(0);
    expect(uploadRefusal(file(UPLOAD_MAX_BYTES, "image/png"))).toBeNull();
    expect(uploadRefusal(file(UPLOAD_MAX_BYTES + 1, "image/png"))).not.toBeNull();
  });

  it("posts the raw file with its own content type and the intent header", async () => {
    const reply = { path: "/run/x/up/abc.png", bytes: 4, mime: "image/png", sha256: "a".repeat(64) };
    replies.push(json(200, reply));
    const f = file(4, "image/png");
    const got = await uploadImage("abcd1234", f);
    expect(got).toEqual(reply);
    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/terms/abcd1234/upload");
    expect(calls[0].init.method).toBe("POST");
    expect(calls[0].init.body).toBe(f);
    expect(headers(calls[0])["Content-Type"]).toBe("image/png");
    expect(headers(calls[0])["X-Kb-Intent"]).toBe("terminal");
  });

  it("hands the caller's AbortSignal to fetch", async () => {
    replies.push(json(200, { path: "/up/a.png", bytes: 4, mime: "image/png", sha256: "a".repeat(64) }));
    const ctl = new AbortController();
    await uploadImage("abcd1234", file(4, "image/png"), ctl.signal);
    expect(calls[0].init.signal).toBe(ctl.signal);
  });

  it("explains a 413 or 415 with the server's own words", async () => {
    replies.push(json(415, { type: "urn:kb:errors:unsupported-media", detail: "not a picture" }));
    await expect(uploadImage("abcd1234", file(4, "image/png"))).rejects.toMatchObject({
      status: 415,
      detail: "not a picture",
    });
  });
});

describe("picture files of a paste or a drop", () => {
  const img = new File([new Uint8Array(4)], "a.png", { type: "image/png" });
  const pdf = new File([new Uint8Array(4)], "a.pdf", { type: "application/pdf" });

  it("a clipboard with one picture and no text is a picture paste", () => {
    expect(pastedPictureFiles(fakeDt({ types: ["Files"], files: [img] }))).toEqual([img]);
  });

  it("a clipboard that also carries text/plain is a TEXT paste (negative control)", () => {
    expect(pastedPictureFiles(fakeDt({ types: ["text/plain", "Files"], files: [img] }))).toEqual([]);
    expect(pastedPictureFiles(fakeDt({ types: ["text/plain"] }))).toEqual([]);
    expect(pastedPictureFiles(null)).toEqual([]);
  });

  it("a non-picture file is not a picture paste", () => {
    expect(pastedPictureFiles(fakeDt({ types: ["Files"], files: [pdf] }))).toEqual([]);
  });

  it("a drop keeps every file so the gate can refuse the non-pictures", () => {
    expect(droppedFiles(fakeDt({ types: ["Files"], files: [img, pdf] }))).toEqual([img, pdf]);
    expect(dragHasFiles(fakeDt({ types: ["Files"] }))).toBe(true);
    expect(dragHasFiles(fakeDt({ types: ["text/plain"] }))).toBe(false);
  });
});
