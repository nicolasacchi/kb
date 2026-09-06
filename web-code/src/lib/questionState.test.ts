import { describe, expect, it } from "vitest";
import type { ReviewComment, ReviewCommentsOut, ReviewFinding } from "../api/types";
import {
  diffAgentReplies,
  isAgentAuthorName,
  latestVoice,
  matchesQuestionFilter,
  questionChipState,
  questionStateForThread,
  threadToastLabel,
  voiceIsAgent,
} from "./questionState";

function comment(overrides: Partial<ReviewComment> = {}): ReviewComment {
  return {
    id: "a1",
    path: "src/lib.rs",
    intent: "note",
    body: "body",
    author: "you",
    created_at: 100,
    updated_at: 100,
    resolved: false,
    anchor_kind: "line",
    side: "new",
    ps_number: 1,
    resolution: {
      line: 10,
      line_end: null,
      orphaned: false,
      resolved_against: { ps: 1, sha: "deadbeef" },
    },
    suggestion: null,
    replies: [],
    ...overrides,
  };
}

function reply(author: string, createdAt: number) {
  return {
    id: `r-${author}-${createdAt}`,
    parent_id: "a1",
    path: "src/lib.rs",
    intent: "note",
    body: "reply",
    author,
    created_at: createdAt,
    updated_at: createdAt,
    resolved: false,
  };
}

function finding(overrides: Partial<ReviewFinding> = {}): ReviewFinding {
  return {
    slug: "f-x",
    severity: "concern",
    category: "correctness",
    location: { kind: "single", path: "src/lib.rs", lines: [10], removed: false },
    title: "t",
    rationale: "r",
    recommendation: null,
    evidence: null,
    origin: "import",
    author: "claude",
    disposition: null,
    published_state: "unpublished",
    published_at: null,
    published_url: null,
    superseded: false,
    superseded_reason: null,
    content_updated_at: null,
    annotation_id: "a1",
    import_batch_id: "b1",
    created_at: 1,
    updated_at: 1,
    resolution: { line: 10, line_end: null, orphaned: false, confidence: "exact" },
    thread_count: 0,
    unresolved_count: 0,
    ...overrides,
  };
}

describe("isAgentAuthorName", () => {
  it("matches 'claude' case/whitespace-insensitively", () => {
    expect(isAgentAuthorName("claude")).toBe(true);
    expect(isAgentAuthorName("Claude")).toBe(true);
    expect(isAgentAuthorName("  claude  ")).toBe(true);
  });
  it("rejects everything else", () => {
    expect(isAgentAuthorName("you")).toBe(false);
    expect(isAgentAuthorName("claude-3")).toBe(false);
    expect(isAgentAuthorName("")).toBe(false);
  });
});

describe("latestVoice", () => {
  it("returns the opener when there are no replies", () => {
    const opener = { author: "you", createdAt: 10 };
    expect(latestVoice(opener, [])).toBe(opener);
  });
  it("returns the reply with the max createdAt, not assuming sort order", () => {
    const opener = { author: "you", createdAt: 10 };
    const r1 = { author: "claude", createdAt: 30 };
    const r2 = { author: "you", createdAt: 20 };
    expect(latestVoice(opener, [r2, r1])).toBe(r1);
  });
});

describe("voiceIsAgent", () => {
  it("falls back to author-string matching when no findingOrigin given", () => {
    expect(voiceIsAgent({ author: "claude", createdAt: 1 }, true)).toBe(true);
    expect(voiceIsAgent({ author: "you", createdAt: 1 }, true)).toBe(false);
  });
  it("prefers findingOrigin over author string, but only for the opener voice", () => {
    // origin says import (agent) even though the author string is atypical.
    expect(voiceIsAgent({ author: "someone-else", createdAt: 1 }, true, "import")).toBe(true);
    // origin says manual (human) even though the author string looks agent-ish.
    expect(voiceIsAgent({ author: "claude", createdAt: 1 }, true, "manual")).toBe(false);
  });
  it("ignores findingOrigin for a non-opener voice (a reply is never itself a finding)", () => {
    expect(voiceIsAgent({ author: "claude", createdAt: 2 }, false, "manual")).toBe(true);
    expect(voiceIsAgent({ author: "you", createdAt: 2 }, false, "import")).toBe(false);
  });
});

describe("questionChipState", () => {
  const base = { intent: "question", resolved: false, opener: { author: "you", createdAt: 100 }, replies: [] };

  it("question, no reply yet → awaiting-agent", () => {
    expect(questionChipState(base)).toBe("awaiting-agent");
  });

  it("question, last reply human (the asker followed up) → awaiting-agent", () => {
    expect(
      questionChipState({ ...base, replies: [{ author: "you", createdAt: 110 }] }),
    ).toBe("awaiting-agent");
  });

  it("last voice agent-authored, unresolved → awaiting-you (regardless of intent)", () => {
    expect(
      questionChipState({ ...base, replies: [{ author: "claude", createdAt: 110 }] }),
    ).toBe("awaiting-you");
    expect(
      questionChipState({
        ...base,
        intent: "note",
        replies: [{ author: "claude", createdAt: 110 }],
      }),
    ).toBe("awaiting-you");
  });

  it("resolved always short-circuits to null, even mid-question with a human last voice", () => {
    expect(questionChipState({ ...base, resolved: true })).toBe(null);
    expect(
      questionChipState({
        ...base,
        resolved: true,
        replies: [{ author: "claude", createdAt: 110 }],
      }),
    ).toBe(null);
  });

  it("a plain note thread with no agent voice is neither state", () => {
    expect(questionChipState({ ...base, intent: "note" })).toBe(null);
  });

  it("an agent-opened finding with no replies reads as awaiting-you via findingOrigin", () => {
    expect(
      questionChipState({
        intent: "note",
        resolved: false,
        opener: { author: "claude", createdAt: 1 },
        replies: [],
        findingOrigin: "import",
      }),
    ).toBe("awaiting-you");
  });
});

describe("questionStateForThread + matchesQuestionFilter", () => {
  it("adapts a ReviewComment + joined finding the same way questionChipState does", () => {
    const t = comment({ intent: "question", replies: [] });
    expect(questionStateForThread(t, null)).toBe("awaiting-agent");
    expect(matchesQuestionFilter("awaiting-agent", t, null)).toBe(true);
    expect(matchesQuestionFilter("awaiting-you", t, null)).toBe(false);
  });

  it("an agent reply flips the filter match", () => {
    const t = comment({ intent: "question", replies: [reply("claude", 200)] });
    expect(questionStateForThread(t, null)).toBe("awaiting-you");
    expect(matchesQuestionFilter("awaiting-you", t, null)).toBe(true);
    expect(matchesQuestionFilter("awaiting-agent", t, null)).toBe(false);
  });
});

describe("threadToastLabel", () => {
  it("prefers the finding slug when the thread is a finding", () => {
    expect(threadToastLabel(comment({ path: "src/lib.rs" }), finding({ slug: "f-pagy" }))).toBe("f-pagy");
  });
  it("labels a review-level (path='') thread as General", () => {
    expect(threadToastLabel(comment({ path: "", resolution: { ...comment().resolution, line: null } }), null)).toBe(
      "General",
    );
  });
  it("labels a plain path thread as path:line", () => {
    expect(threadToastLabel(comment({ path: "a.rb" }), null)).toBe("a.rb:10");
  });
  it("falls back to bare path when there is no live line", () => {
    expect(
      threadToastLabel(comment({ path: "a.rb", resolution: { ...comment().resolution, line: null } }), null),
    ).toBe("a.rb");
  });
});

function commentsOut(groups: { path: string; comments: ReviewComment[] }[]): ReviewCommentsOut {
  return { schema: "review-comments/1", review_id: 7, repo: "r", ps: 1, groups };
}

describe("diffAgentReplies", () => {
  it("seedOnly never emits toasts, but still seeds the watermark map", () => {
    const data = commentsOut([
      { path: "a.rb", comments: [comment({ id: "t1", path: "a.rb", replies: [reply("claude", 200)] })] },
    ]);
    const { toasts, nextByThread } = diffAgentReplies(new Map(), data, new Map(), true);
    expect(toasts).toEqual([]);
    expect(nextByThread.get("t1")).toBe(200);
  });

  it("emits a toast for a NEW agent voice not seen in prevByThread's watermark", () => {
    const data = commentsOut([
      { path: "a.rb", comments: [comment({ id: "t1", path: "a.rb", replies: [reply("claude", 200)] })] },
    ]);
    const { toasts } = diffAgentReplies(new Map([["t1", 100]]), data, new Map(), false);
    expect(toasts).toEqual([{ threadId: "t1", label: "a.rb:10" }]);
  });

  it("does not re-toast a voice already at or before the watermark", () => {
    const data = commentsOut([
      { path: "a.rb", comments: [comment({ id: "t1", path: "a.rb", replies: [reply("claude", 200)] })] },
    ]);
    const { toasts } = diffAgentReplies(new Map([["t1", 200]]), data, new Map(), false);
    expect(toasts).toEqual([]);
  });

  it("never toasts a human (your own) action — dedupe via author", () => {
    const data = commentsOut([
      { path: "a.rb", comments: [comment({ id: "t1", author: "you", replies: [reply("you", 200)] })] },
    ]);
    const { toasts } = diffAgentReplies(new Map([["t1", 100]]), data, new Map(), false);
    expect(toasts).toEqual([]);
  });

  it("labels a finding-backed thread's toast with its slug", () => {
    const t = comment({ id: "a1", replies: [reply("claude", 200)] });
    const data = commentsOut([{ path: "a.rb", comments: [t] }]);
    const findings = new Map([["a1", finding({ slug: "f-pagy", origin: "import" })]]);
    const { toasts } = diffAgentReplies(new Map([["a1", 100]]), data, findings, false);
    expect(toasts).toEqual([{ threadId: "a1", label: "f-pagy" }]);
  });
});
