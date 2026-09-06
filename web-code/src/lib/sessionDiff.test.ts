import { describe, expect, it } from "vitest";
import type { CommitEntryOut, SessionDiff } from "../api/types";
import {
  commitFileStat,
  commitShortSha,
  isCommitsSegment,
  isPromptSegment,
  isUncommittedSegment,
  siblingFiles,
  totalsSummary,
} from "./sessionDiff";

function commit(overrides: Partial<CommitEntryOut> = {}): CommitEntryOut {
  return {
    sha: "deadbeefcafe",
    trailers: [],
    diffed: true,
    files: [],
    insertions: 0,
    deletions: 0,
    ...overrides,
  };
}

function diff(overrides: Partial<SessionDiff> = {}): SessionDiff {
  return {
    version: "session-diff/1",
    session_id: "sess-1",
    segments: [],
    repos_touched: [],
    totals: { commits: 0, commits_diffed: 0, files: 0, insertions: 0, deletions: 0 },
    commits_status: { status: "ok" },
    ...overrides,
  };
}

describe("siblingFiles", () => {
  it("unions committed + uncommitted files, deduped, excluding the current path", () => {
    const d = diff({
      segments: [
        {
          kind: "commits",
          commits: [
            commit({
              files: [
                { path: "src/a.rs", insertions: 1, deletions: 0, binary: false },
                { path: "src/b.rs", insertions: 2, deletions: 1, binary: false },
              ],
            }),
          ],
        },
        { kind: "uncommitted", files: ["/repo/src/b.rs", "/repo/src/c.rs"], turns: [] },
      ],
    });
    expect(siblingFiles(d, "src/a.rs", "/repo")).toEqual(["src/b.rs", "src/c.rs"]);
  });

  it("leaves uncommitted paths absolute when no repoRoot is given (degraded, not dropped)", () => {
    const d = diff({ segments: [{ kind: "uncommitted", files: ["/repo/src/c.rs"], turns: [] }] });
    expect(siblingFiles(d, "src/a.rs")).toEqual(["/repo/src/c.rs"]);
  });

  it("returns an empty list when the session only touched the excluded path", () => {
    const d = diff({
      segments: [
        { kind: "commits", commits: [commit({ files: [{ path: "src/a.rs", insertions: 1, deletions: 0, binary: false }] })] },
      ],
    });
    expect(siblingFiles(d, "src/a.rs", "/repo")).toEqual([]);
  });

  it("ignores prompt segments entirely", () => {
    const d = diff({ segments: [{ kind: "prompt", ts: 1, uuid: "u1", text: "do the thing" }] });
    expect(siblingFiles(d, "src/a.rs")).toEqual([]);
  });

  it("does not throw on an undiffed commit whose files/trailers keys are entirely absent", () => {
    // Mirrors the real wire shape: server's `commit_entry_unresolved` (and
    // any diffed commit with zero changed files) OMITS `files`/`trailers`
    // rather than sending `[]` (`skip_serializing_if = "Vec::is_empty"`,
    // `CommitEntryOut`'s doc in `api/types.ts`) — no `commit()` helper here
    // so those keys are truly missing, not defaulted to `[]`.
    const undiffed: CommitEntryOut = {
      sha: "cafebabe1234",
      diffed: false,
      insertions: 0,
      deletions: 0,
    };
    const d = diff({
      segments: [
        { kind: "commits", commits: [undiffed] },
        { kind: "uncommitted", files: ["/repo/src/z.rs"], turns: [] },
      ],
    });
    expect(() => siblingFiles(d, "src/a.rs", "/repo")).not.toThrow();
    expect(siblingFiles(d, "src/a.rs", "/repo")).toEqual(["src/z.rs"]);
  });
});

describe("commitFileStat", () => {
  it("formats a text file's insertions/deletions", () => {
    expect(commitFileStat({ path: "a.rs", insertions: 3, deletions: 1, binary: false })).toBe("+3 -1");
  });

  it("reports binary files without a line count", () => {
    expect(commitFileStat({ path: "a.png", insertions: 0, deletions: 0, binary: true })).toBe("binary");
  });
});

describe("commitShortSha", () => {
  it("truncates to 7 characters", () => {
    expect(commitShortSha(commit({ sha: "0123456789abcdef" }))).toBe("0123456");
  });
});

describe("totalsSummary", () => {
  it("reports a plain commit count when every commit diffed", () => {
    const summary = totalsSummary({ commits: 3, commits_diffed: 3, files: 2, insertions: 5, deletions: 1 });
    expect(summary).toBe("3 commits · 2 files · +5 -1");
  });

  it("reports the diffed/total split when some commits couldn't be diffed", () => {
    const summary = totalsSummary({ commits: 3, commits_diffed: 1, files: 2, insertions: 5, deletions: 1 });
    expect(summary).toBe("1/3 commits diffed · 2 files · +5 -1");
  });

  it("singularizes a single commit/file", () => {
    const summary = totalsSummary({ commits: 1, commits_diffed: 1, files: 1, insertions: 1, deletions: 0 });
    expect(summary).toBe("1 commit · 1 file · +1 -0");
  });
});

describe("segment type guards", () => {
  it("narrow by kind", () => {
    const prompt: Parameters<typeof isPromptSegment>[0] = { kind: "prompt", ts: 1, uuid: "u", text: "t" };
    const commits: Parameters<typeof isCommitsSegment>[0] = { kind: "commits", commits: [] };
    const uncommitted: Parameters<typeof isUncommittedSegment>[0] = { kind: "uncommitted", files: [], turns: [] };
    expect(isPromptSegment(prompt)).toBe(true);
    expect(isPromptSegment(commits)).toBe(false);
    expect(isCommitsSegment(commits)).toBe(true);
    expect(isUncommittedSegment(uncommitted)).toBe(true);
    expect(isUncommittedSegment(prompt)).toBe(false);
  });
});
