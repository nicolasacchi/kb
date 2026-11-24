import { describe, expect, it } from "vitest";
import type { SessionRow } from "../../api/generated/SessionRow";
import { groupCwdsByProject, isPlainResumeToken, resumableRows, resumeRefusal } from "./launchResume";

const row = (p: Partial<SessionRow>): SessionRow =>
  ({ session_id: "s1", harness: "claude", cwd: "/w/a", started_at: 10, display_name: "n", ...p }) as SessionRow;

describe("isPlainResumeToken", () => {
  it.each([
    ["abc", true],
    ["a", true],
    ["a.b_c:d-e", true],
    ["0123-abcd", true],
    ["a".repeat(128), true],
    ["a".repeat(129), false],
    ["", false],
    ["-abc", false],
    [".abc", false],
    ["_abc", false],
    ["a b", false],
    ["a/b", false],
    ["a;b", false],
    ["a\nb", false],
    ["é", false],
  ])("%j -> %s", (s, ok) => expect(isPlainResumeToken(s)).toBe(ok));
});

describe("resumableRows", () => {
  it("offers the five resumable harnesses and drops kimi, no-cwd, bad cwd and bad token rows", () => {
    const rows = [
      ...["claude", "codex", "omp", "opencode", "grok"].map((h) => row({ harness: h, session_id: `ok-${h}` })),
      row({ harness: "kimi", session_id: "k" }),
      row({ session_id: "nocwd", cwd: undefined }),
      row({ session_id: "rel", cwd: "relative/dir" }),
      row({ session_id: "-bad" }),
      row({ session_id: "ctl", cwd: "/a\u0000b" }),
    ];
    expect(resumableRows(rows).map((r) => r.session_id)).toEqual([
      "ok-claude",
      "ok-codex",
      "ok-omp",
      "ok-opencode",
      "ok-grok",
    ]);
  });
  it("names a refusal reason for each dropped row", () => {
    expect(resumeRefusal(row({ harness: "kimi" }))).toMatch(/harness/);
    expect(resumeRefusal(row({ cwd: undefined }))).toMatch(/no working directory/);
    expect(resumeRefusal(row({}))).toBeNull();
  });
});

describe("groupCwdsByProject", () => {
  it("keeps the most recent cwd per project, newest group first, dropping invalid cwds", () => {
    const out = groupCwdsByProject([
      row({ project_key: "p1", cwd: "/old", started_at: 1 }),
      row({ project_key: "p1", cwd: "/new", started_at: 5 }),
      row({ project_key: "p2", cwd: "/two", started_at: 9 }),
      row({ project_key: "p3", cwd: "nope", started_at: 99 }),
      row({ project_key: undefined, folder: "f", cwd: "/f", started_at: 3 }),
    ]);
    expect(out).toEqual([
      { group: "p2", cwd: "/two" },
      { group: "p1", cwd: "/new" },
      { group: "f", cwd: "/f" },
    ]);
  });
});
