import { describe, it, expect } from "vitest";
import type { ReviewFile } from "../api/client";
import {
  embedReviewIntoHtml,
  extractReviewFromHtml,
  KB_REVIEW_STATE_ID,
} from "./reviewEmbed";

function fixture(body = "hello"): ReviewFile {
  return {
    schema: "kb-comments/1",
    artifact: { id: "abc123def456", title: "T", kb: "smoke", tags: [], pages: [] },
    generatedAt: "2026-05-12T10:00:00Z",
    comments: [
      {
        id: "c_1",
        status: "open",
        file: "abc123def456",
        fileLabel: "main",
        anchor: { kind: "file" },
        author: "you",
        body,
        createdAt: "2026-05-12T10:05:00Z",
        editedAt: null,
        replies: [],
        choices: [],
        attachments: [],
      },
    ],
  } as unknown as ReviewFile;
}

describe("embedReviewIntoHtml / extractReviewFromHtml", () => {
  const html =
    "<!doctype html><html><head><title>x</title></head><body><p>hi</p></body></html>";

  it("round-trips a review file through an embedded HTML copy", () => {
    const embedded = embedReviewIntoHtml(html, fixture());
    expect(embedded).toContain(KB_REVIEW_STATE_ID);
    // The block lands inside <head>.
    expect(embedded.slice(0, embedded.indexOf("</head>"))).toContain(
      KB_REVIEW_STATE_ID,
    );
    const back = extractReviewFromHtml(embedded);
    expect(back).not.toBeNull();
    expect(back!.comments).toHaveLength(1);
    expect(back!.comments[0].id).toBe("c_1");
    expect(back!.comments[0].status).toBe("open");
  });

  it("is idempotent — re-embedding replaces, never appends", () => {
    const once = embedReviewIntoHtml(html, fixture());
    const twice = embedReviewIntoHtml(once, fixture());
    expect(twice.split(KB_REVIEW_STATE_ID)).toHaveLength(2); // one occurrence
  });

  it("escapes a literal </script> in a comment body", () => {
    const embedded = embedReviewIntoHtml(html, fixture("watch </script> out"));
    // Only the real closing tag remains.
    expect(embedded.split("</script>")).toHaveLength(2);
    const back = extractReviewFromHtml(embedded);
    expect(back!.comments[0].body).toBe("watch </script> out");
  });

  it("returns null when no block is present", () => {
    expect(extractReviewFromHtml(html)).toBeNull();
  });

  it("throws on an unsupported embedded schema", () => {
    const bad =
      `<html><head><script type="application/json" id="${KB_REVIEW_STATE_ID}">` +
      `{"schema":"kb-comments/99","artifact":{"id":"x","title":"y","kb":"z"},` +
      `"generatedAt":"2026-05-12T10:00:00Z","comments":[]}</script></head><body></body></html>`;
    expect(() => extractReviewFromHtml(bad)).toThrow(/schema/);
  });
});
