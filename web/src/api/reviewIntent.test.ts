// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import {
  OPERATOR_INTENT_HEADER,
  announceReviewMutation,
  onReviewMutation,
  reviewMutationTarget,
  withOperatorIntent,
} from "./reviewIntent";
import { attachmentServeUrl } from "../lib/attachmentUrl";

describe("operator intent on review mutations (v0.44 P2)", () => {
  it("adds the header on review paths only", () => {
    expect(
      withOperatorIntent("/api/kb/k/review/abc/comments/c_1/resolve", {})[
        OPERATOR_INTENT_HEADER
      ],
    ).toBe("all");
    expect(
      withOperatorIntent("/api/kb/k/artifacts/abc/meta", {})[OPERATOR_INTENT_HEADER],
    ).toBeUndefined();
  });

  it("decodes the kb and artifact from a review path", () => {
    expect(reviewMutationTarget("/api/kb/my%20kb/review/abc123/import?force=true")).toEqual({
      kb: "my kb",
      artifact_id: "abc123",
    });
    expect(reviewMutationTarget("/api/kb/k/docs")).toBeNull();
  });

  it("announces a review mutation to this tab's listeners, and only for review paths", () => {
    const fn = vi.fn();
    const off = onReviewMutation(fn);
    announceReviewMutation("/api/kb/k/review/abc/comments/c_1/meta");
    announceReviewMutation("/api/kb/k/artifacts/abc/meta");
    off();
    announceReviewMutation("/api/kb/k/review/abc/comments");
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith({ kb: "k", artifact_id: "abc" });
  });

  it("attachment URLs carry the operator's read opt-in", () => {
    expect(attachmentServeUrl("k", "abc", "a_1")).toMatch(
      /\/api\/kb\/k\/review\/abc\/attachments\/a_1\?visibility=all$/,
    );
  });
});
