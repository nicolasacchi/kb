import { afterEach, describe, expect, it, vi } from "vitest";
import { captureUpload } from "./capture";

// U4 — the capture() multipart wire shape: which fields get sent (and which
// are omitted vs. explicit) is the actual contract with routes/capture.rs's
// `drain_capture_form`.

function mockFetch(
  status = 201,
  body: unknown = { items: [] },
  contentType = "application/json",
) {
  const calls: { url: string; init?: RequestInit }[] = [];
  const fn = vi.fn(async (url: string | URL, init?: RequestInit) => {
    calls.push({ url: String(url), init });
    return new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": contentType },
    });
  });
  vi.stubGlobal("fetch", fn);
  return calls;
}

describe("captureUpload()", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("POSTs multipart files to /api/kb/{kb}/capture with X-Requested-By: kb-spa", async () => {
    const calls = mockFetch();
    const file = new File(["hello"], "note.md", { type: "text/markdown" });
    await captureUpload("canon", { files: [file] });

    expect(calls).toHaveLength(1);
    expect(calls[0].url).toBe("/api/kb/canon/capture");
    const init = calls[0].init!;
    expect(init.method).toBe("POST");
    expect((init.headers as Record<string, string>)["X-Requested-By"]).toBe(
      "kb-spa",
    );
    expect(init.headers).not.toHaveProperty("Content-Type");
    const form = init.body as FormData;
    const files = form.getAll("files");
    expect(files).toHaveLength(1);
    expect((files[0] as File).name).toBe("note.md");
  });

  it("appends multiple files under the same `files` field", async () => {
    const calls = mockFetch();
    await captureUpload("canon", {
      files: [
        new File(["a"], "a.md"),
        new File(["b"], "b.html", { type: "text/html" }),
      ],
    });
    const form = calls[0].init!.body as FormData;
    expect(form.getAll("files").map((f) => (f as File).name)).toEqual([
      "a.md",
      "b.html",
    ]);
  });

  it("sends title/tags/sanitize only when explicitly set", async () => {
    const calls = mockFetch();
    await captureUpload("canon", {
      files: [new File(["x"], "a.txt")],
      title: "My Title",
      tags: ["research", "phone"],
      sanitize: true,
    });
    const form = calls[0].init!.body as FormData;
    expect(form.get("title")).toBe("My Title");
    expect(form.get("tags")).toBe("research,phone");
    expect(form.get("sanitize")).toBe("true");
  });

  it("omits title/tags/sanitize when unset (server takes its own default)", async () => {
    const calls = mockFetch();
    await captureUpload("canon", { files: [new File(["x"], "a.txt")] });
    const form = calls[0].init!.body as FormData;
    expect(form.has("title")).toBe(false);
    expect(form.has("tags")).toBe(false);
    expect(form.has("sanitize")).toBe(false);
  });

  it("sends an explicit sanitize:false distinctly from omitted", async () => {
    const calls = mockFetch();
    await captureUpload("canon", {
      files: [new File(["x"], "a.txt")],
      sanitize: false,
    });
    const form = calls[0].init!.body as FormData;
    expect(form.get("sanitize")).toBe("false");
  });

  it("resolves the typed CaptureResponse on 201", async () => {
    mockFetch(201, {
      items: [
        { kb: "canon", id: "abc123", source_relative: "capture/a-1.md", title: "a" },
      ],
    });
    const resp = await captureUpload("canon", {
      files: [new File(["x"], "a.md")],
    });
    expect(resp.items).toHaveLength(1);
    expect(resp.items[0]).toMatchObject({
      kb: "canon",
      source_relative: "capture/a-1.md",
    });
  });

  it("throws a problem+json-aware error (title + detail) on failure", async () => {
    mockFetch(
      415,
      {
        type: "about:blank",
        title: "Unsupported Media Type",
        status: 415,
        detail: "`svg` has no indexable extension for this kb",
      },
      "application/problem+json",
    );
    await expect(
      captureUpload("canon", { files: [new File(["x"], "a.svg")] }),
    ).rejects.toThrow(/Unsupported Media Type.*no indexable extension/);
  });

  it("falls back to the status line when the body isn't problem+json", async () => {
    mockFetch(500, "boom", "text/plain");
    await expect(
      captureUpload("canon", { files: [new File(["x"], "a.md")] }),
    ).rejects.toThrow(/500/);
  });
});
