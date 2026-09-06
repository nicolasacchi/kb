import { describe, expect, it } from "vitest";
import { combineConversationState } from "./GithubConversationCard";

function q<T>(overrides: Partial<{ isLoading: boolean; data: T | undefined }>) {
  return { isLoading: false, data: undefined, ...overrides };
}

describe("combineConversationState", () => {
  it("is 'loading' when either query is still loading", () => {
    expect(combineConversationState(q({ isLoading: true }), q({})).kind).toBe("loading");
    expect(combineConversationState(q({}), q({ isLoading: true })).kind).toBe("loading");
  });

  it("is 'unavailable' with the reviews reason when reviews degraded", () => {
    const state = combineConversationState(
      q({ data: { reviewers: [], unavailable_reason: "rate-limited" } }),
      q({ data: { comments: [] } }),
    );
    expect(state).toEqual({ kind: "unavailable", reason: "rate-limited" });
  });

  it("is 'unavailable' with the comments reason when only comments degraded", () => {
    const state = combineConversationState(
      q({ data: { reviewers: [] } }),
      q({ data: { comments: [], unavailable_reason: "timeout" } }),
    );
    expect(state).toEqual({ kind: "unavailable", reason: "timeout" });
  });

  it("is 'empty' when both landed successfully with nothing to show", () => {
    const state = combineConversationState(
      q({ data: { reviewers: [] } }),
      q({ data: { comments: [] } }),
    );
    expect(state.kind).toBe("empty");
  });

  it("is 'ready' with both lists when either has content", () => {
    const state = combineConversationState(
      q({ data: { reviewers: [{ reviewer: "octocat", state: "APPROVED", submitted_at: null }] } }),
      q({ data: { comments: [] } }),
    );
    expect(state.kind).toBe("ready");
    if (state.kind === "ready") {
      expect(state.reviewers).toHaveLength(1);
    }
  });

  it("degrades to empty arrays when a query's data is undefined but not loading (shouldn't normally happen)", () => {
    const state = combineConversationState(q({}), q({}));
    expect(state.kind).toBe("empty");
  });
});
