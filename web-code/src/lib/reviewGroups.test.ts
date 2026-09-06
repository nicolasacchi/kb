import { describe, expect, it } from "vitest";
import { distinctAttributedSessionCount, defaultGroupedView, groupCommitsBySession } from "./reviewGroups";
import type { CompareCommitOut, LadderAttribution } from "../api/types";

function attrib(overrides: Partial<LadderAttribution> = {}): LadderAttribution {
  return {
    schema: "join/1",
    confidence: "exact",
    via: "test",
    sha: overrides.sha ?? "deadbeef",
    ...overrides,
  };
}

function commit(overrides: Partial<CompareCommitOut> = {}): CompareCommitOut {
  return {
    sha: "sha1",
    subject: "a commit",
    author: "Test <test@example.com>",
    author_time: 1000,
    ...overrides,
  };
}

describe("distinctAttributedSessionCount", () => {
  it("is 0 for commits with no attribution at all", () => {
    expect(distinctAttributedSessionCount([commit(), commit()])).toBe(0);
  });

  it("is 0 for an honest confidence:none miss even with a session_id present", () => {
    const c = commit({ attribution: attrib({ confidence: "none", session_id: "s1" }) });
    expect(distinctAttributedSessionCount([c])).toBe(0);
  });

  it("is 0 for a confident attribution missing a session_id", () => {
    const c = commit({ attribution: attrib({ confidence: "fuzzy", session_id: undefined }) });
    expect(distinctAttributedSessionCount([c])).toBe(0);
  });

  it("counts one distinct session across several commits sharing it", () => {
    const commits = [
      commit({ sha: "a", attribution: attrib({ session_id: "s1" }) }),
      commit({ sha: "b", attribution: attrib({ session_id: "s1" }) }),
    ];
    expect(distinctAttributedSessionCount(commits)).toBe(1);
  });

  it("counts two distinct sessions", () => {
    const commits = [
      commit({ sha: "a", attribution: attrib({ session_id: "s1" }) }),
      commit({ sha: "b", attribution: attrib({ session_id: "s2" }) }),
    ];
    expect(distinctAttributedSessionCount(commits)).toBe(2);
  });
});

describe("defaultGroupedView", () => {
  it("is false for zero attributed sessions", () => {
    expect(defaultGroupedView([commit(), commit()])).toBe(false);
  });

  it("is false for exactly one distinct attributed session", () => {
    const commits = [
      commit({ sha: "a", attribution: attrib({ session_id: "s1" }) }),
      commit({ sha: "b", attribution: attrib({ session_id: "s1" }) }),
    ];
    expect(defaultGroupedView(commits)).toBe(false);
  });

  it("is true once >= 2 distinct attributed sessions are present", () => {
    const commits = [
      commit({ sha: "a", attribution: attrib({ session_id: "s1" }) }),
      commit({ sha: "b", attribution: attrib({ session_id: "s2" }) }),
    ];
    expect(defaultGroupedView(commits)).toBe(true);
  });
});

describe("groupCommitsBySession", () => {
  it("pools every unattributed commit into one null-keyed bucket, preserving input order", () => {
    const commits = [
      commit({ sha: "a", author_time: 300 }),
      commit({ sha: "b", author_time: 100, attribution: attrib({ confidence: "none" }) }),
      commit({ sha: "c", author_time: 200 }),
    ];
    const groups = groupCommitsBySession(commits);
    expect(groups).toHaveLength(1);
    expect(groups[0].sessionId).toBeNull();
    expect(groups[0].displayName).toBeNull();
    expect(groups[0].confidence).toBeNull();
    expect(groups[0].commits.map((c) => c.sha)).toEqual(["a", "b", "c"]);
    expect(groups[0].earliestAuthorTime).toBe(100);
  });

  it("splits commits into their own attributed session's group", () => {
    const commits = [
      commit({ sha: "a", author_time: 100, attribution: attrib({ session_id: "s1", display_name: "Session One" }) }),
      commit({ sha: "b", author_time: 200, attribution: attrib({ session_id: "s2", display_name: "Session Two" }) }),
      commit({ sha: "c", author_time: 150, attribution: attrib({ session_id: "s1" }) }),
    ];
    const groups = groupCommitsBySession(commits);
    expect(groups).toHaveLength(2);

    const s1 = groups.find((g) => g.sessionId === "s1");
    const s2 = groups.find((g) => g.sessionId === "s2");
    expect(s1?.commits.map((c) => c.sha)).toEqual(["a", "c"]);
    expect(s1?.displayName).toBe("Session One");
    expect(s2?.commits.map((c) => c.sha)).toEqual(["b"]);
  });

  it("orders groups by their own earliest commit's author_time ascending", () => {
    const commits = [
      commit({ sha: "a", author_time: 500, attribution: attrib({ session_id: "late-session" }) }),
      commit({ sha: "b", author_time: 100, attribution: attrib({ session_id: "early-session" }) }),
      commit({ sha: "c", author_time: 300 }), // unattributed, lands between the two by time
    ];
    const groups = groupCommitsBySession(commits);
    expect(groups.map((g) => g.sessionId)).toEqual(["early-session", null, "late-session"]);
  });

  it("badges a group with the STRONGEST confidence among its own commits", () => {
    const commits = [
      commit({ sha: "a", author_time: 100, attribution: attrib({ session_id: "s1", confidence: "fuzzy" }) }),
      commit({ sha: "b", author_time: 200, attribution: attrib({ session_id: "s1", confidence: "trailer" }) }),
    ];
    const groups = groupCommitsBySession(commits);
    expect(groups[0].confidence).toBe("trailer");
  });

  it("falls back to the session_id as displayName when no display_name is ever attached", () => {
    const commits = [commit({ sha: "a", attribution: attrib({ session_id: "s1", display_name: undefined }) })];
    const groups = groupCommitsBySession(commits);
    expect(groups[0].displayName).toBe("s1");
  });

  it("returns an empty group list for an empty commit list", () => {
    expect(groupCommitsBySession([])).toEqual([]);
  });
});
