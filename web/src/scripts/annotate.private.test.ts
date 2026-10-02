// @vitest-environment jsdom
//
// v0.44 P2 — the in-iframe annotator never renders a private note, even when
// one reaches it. The daemon (`?cm=on` payload) and the SPA bridge
// (`publicView`) both project notes away first; this pins the third,
// defensive layer in `annotate.ts` itself.
import { afterEach, describe, expect, it, vi } from "vitest";

const base = {
  status: "open",
  file: "art1",
  fileLabel: "art1",
  anchor: { kind: "file" },
  author: "you",
  createdAt: "2026-01-01T00:00:00Z",
  editedAt: null,
  replies: [],
};

afterEach(() => {
  document.body.innerHTML = "";
  delete (window as unknown as Record<string, unknown>).__KB_COMMENTS;
  vi.resetModules();
});

describe("annotate.ts private-note defence", () => {
  it("paints the public comment and ignores a private one that arrives", async () => {
    (window as unknown as Record<string, unknown>).__KB_COMMENTS = {
      v: 1,
      etag: null,
      file: {
        schema: "kb-comments/1",
        artifact: { id: "art1", title: "", kb: "k" },
        generatedAt: "2026-01-01T00:00:00Z",
        comments: [
          { ...base, id: "pub1", body: "public body" },
          { ...base, id: "priv1", body: "SECRET-NOTE-BODY", private: true },
        ],
      },
    };
    vi.resetModules();
    await import("./annotate");
    const ids = Array.from(
      document.querySelectorAll("[data-kb-comment-id]"),
    ).map((el) => (el as HTMLElement).dataset.kbCommentId);
    expect(ids).toContain("pub1");
    expect(ids).not.toContain("priv1");
    expect(document.body.innerHTML).not.toContain("SECRET-NOTE-BODY");
  });
});
