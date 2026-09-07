import { describe, expect, it } from "vitest";
import type { ClaimOut } from "../api/types";
import {
  claimKindLabel,
  claimProvenanceText,
  claimsRenderOrder,
  confidenceText,
  evidenceRefView,
  ladderLabel,
} from "./claims";

function claim(overrides: Partial<ClaimOut> = {}): ClaimOut {
  return {
    schema: "kbc-claim/1",
    id: "c-1",
    repo: "r",
    subject_kind: "path",
    subject: "a.rb",
    kind: "explain",
    body_md: "this is the retry path",
    evidence: [],
    state: "pinned",
    caption: "matches the current blob",
    created_at: 100,
    ...overrides,
  };
}

describe("claimKindLabel", () => {
  it("maps every closed kind to a human label", () => {
    expect(claimKindLabel("explain")).toBe("explains");
    expect(claimKindLabel("alternative")).toBe("alternative considered");
    expect(claimKindLabel("decision")).toBe("decision");
    expect(claimKindLabel("story")).toBe("branch story");
    expect(claimKindLabel("note")).toBe("note");
    expect(claimKindLabel("answer")).toBe("answer");
  });

  it("degrades an unrecognized kind to itself, never crashes", () => {
    expect(claimKindLabel("something_future")).toBe("something_future");
  });
});

describe("confidenceText — the agent's own declaration, verbatim, never a bar", () => {
  it("renders a stated confidence", () => {
    expect(confidenceText(0.7)).toBe("agent-declared 0.7");
    expect(confidenceText(1)).toBe("agent-declared 1");
    expect(confidenceText(0)).toBe("agent-declared 0");
  });

  it("names an absent confidence honestly rather than a blank", () => {
    expect(confidenceText(undefined)).toBe("agent-declared — not stated");
  });
});

describe("ladderLabel", () => {
  it("covers the three ladder states", () => {
    expect(ladderLabel("pinned")).toBe("pinned");
    expect(ladderLabel("drifted")).toBe("drifted");
    expect(ladderLabel("unanchored")).toBe("unanchored");
  });

  it("degrades an unrecognized state to itself", () => {
    expect(ladderLabel("weird")).toBe("weird");
  });
});

describe("claimProvenanceText", () => {
  it("names model + session when both are present", () => {
    expect(claimProvenanceText({ model: "claude-x", session_id: "0189aa112233abcdef" })).toBe(
      "claude-x · session 0189aa112233",
    );
  });

  it("names only what it has", () => {
    expect(claimProvenanceText({ model: "claude-x" })).toBe("claude-x");
    expect(claimProvenanceText({ session_id: "0189aa112233" })).toBe("session 0189aa112233");
  });

  it("names an honest fallback when neither is present (a human-authored claim)", () => {
    expect(claimProvenanceText({})).toBe("no session recorded");
  });
});

describe("evidenceRefView — small evidence cards, never a fabricated link", () => {
  it("code: ref links into the diff when a reviewId is known", () => {
    const v = evidenceRefView("code:app/models/order.rb:12", "acme", 42);
    expect(v.raw).toBe("code:app/models/order.rb:12");
    expect(v.href).toBe("/r/acme/~reviews/42/diff/app/models/order.rb");
  });

  it("code: ref falls back to the plain reader when no reviewId is known", () => {
    const v = evidenceRefView("code:app/models/order.rb:12", "acme", undefined);
    expect(v.href).not.toBeNull();
    expect(v.href).toContain("app/models/order.rb");
  });

  it("finding: ref links only when a reviewId is known", () => {
    const withReview = evidenceRefView("finding:f-a", "acme", 42);
    expect(withReview.href).toBe("/r/acme/~reviews/42/diff?finding=f-a");
    const withoutReview = evidenceRefView("finding:f-a", "acme", undefined);
    expect(withoutReview.href).toBeNull();
  });

  it("gh:/kb: refs are named but never linked — no host this register can resolve", () => {
    expect(evidenceRefView("gh:comment/12", "acme", 42).href).toBeNull();
    expect(evidenceRefView("kb:research/9f8b7182", "acme", 42).href).toBeNull();
  });

  it("an unparseable ref degrades to plain text, never throws", () => {
    const v = evidenceRefView("not a kbc ref at all", "acme", 42);
    expect(v.href).toBeNull();
    expect(v.label).toBe("not a kbc ref at all");
  });
});

describe("claimsRenderOrder — surfaced, never scored", () => {
  it("is the identity function: same array, same order, no sort", () => {
    const claims = [
      claim({ id: "c-3", confidence: 0.1 }),
      claim({ id: "c-1", confidence: 0.9 }),
      claim({ id: "c-2", confidence: 0.5 }),
    ];
    const out = claimsRenderOrder(claims);
    expect(out).toBe(claims); // referential identity — not even a copy
    expect(out.map((c) => c.id)).toEqual(["c-3", "c-1", "c-2"]);
  });
});
