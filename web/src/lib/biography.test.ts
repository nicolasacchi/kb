import { describe, expect, it } from "vitest";
import {
  BIOGRAPHY_VISIBLE_CAP,
  assembleBiography,
  splitBiography,
  type BioEvent,
} from "./biography";
import type { ArtifactSessionOut } from "../api/sessions";
import type { Comment } from "../api/client";
import type { Version } from "../api/versions";

function session(over: Partial<ArtifactSessionOut>): ArtifactSessionOut {
  return {
    session_id: "s1",
    kb: "kb",
    started_at: 1000,
    display_name: "Session One",
    action: "read",
    read: true,
    wrote: false,
    edited: false,
    authored: false,
    first_user_prompt: undefined,
    decisions: [],
    commits: [],
    ...over,
  } as ArtifactSessionOut;
}

function comment(over: Partial<Comment>): Comment {
  return {
    id: "c1",
    status: "open",
    file: "a.html",
    fileLabel: "a.html",
    anchor: { kind: "file" },
    author: "you",
    body: "hi",
    createdAt: "1970-01-01T00:16:40.000Z", // 1000s
    editedAt: null,
    replies: [],
    choices: [],
    attachments: [],
    ...over,
  } as Comment;
}

function version(over: Partial<Version>): Version {
  return {
    ref: "abc123",
    source: "git",
    label: "fix: x",
    author: "me",
    ts_unix: 500,
    short: "abc123",
    ...over,
  } as Version;
}

describe("assembleBiography", () => {
  it("returns nothing for empty inputs", () => {
    expect(assembleBiography({ sessions: [], comments: [], versions: [] })).toEqual(
      [],
    );
  });

  it("sorts every kind newest-first by unix", () => {
    const events = assembleBiography({
      sessions: [
        session({ session_id: "old", started_at: 100 }),
        session({ session_id: "new", started_at: 900 }),
      ],
      comments: [comment({ createdAt: new Date(500_000).toISOString() })],
      versions: [version({ ref: "v1", ts_unix: 700 })],
    });
    expect(events.map((e) => e.unix)).toEqual([900, 700, 500, 100]);
  });

  it("emits `origin` for an authored session and `session` otherwise", () => {
    const events = assembleBiography({
      sessions: [
        session({ session_id: "birth", authored: true, wrote: true }),
        session({ session_id: "later", authored: false, read: true }),
      ],
      comments: [],
      versions: [],
    });
    const kinds = events.map((e) => e.kind);
    expect(kinds).toContain("origin");
    expect(kinds).toContain("session");
    const origin = events.find((e) => e.kind === "origin");
    expect(origin && "sessionId" in origin && origin.sessionId).toBe("birth");
  });

  it("expands decisions and commits under their session, sharing its unix", () => {
    const events = assembleBiography({
      sessions: [
        session({
          session_id: "s1",
          started_at: 300,
          decisions: [{ kind: "plan", prompt: "do the thing", answer: "yes" }],
          commits: [
            {
              kind: "commit",
              sha: "abc1234",
              subject: "fix bug",
              resolved: true,
              trailers: [],
            },
          ],
        }),
      ],
      comments: [],
      versions: [],
    });
    expect(events.map((e) => e.kind)).toEqual(["session", "decision", "commit"]);
    expect(events.every((e) => e.unix === 300)).toBe(true);
    const decision = events[1];
    expect(decision.kind === "decision" && decision.prompt).toBe("do the thing");
    const commit = events[2];
    expect(commit.kind === "commit" && commit.sha).toBe("abc1234");
    expect(commit.kind === "commit" && commit.subject).toBe("fix bug");
  });

  it("never labels an unresolved commit with a subject, even if the row has one", () => {
    const events = assembleBiography({
      sessions: [
        session({
          commits: [
            {
              kind: "commit",
              sha: "def5678",
              subject: "should not surface",
              resolved: false,
              trailers: [],
            },
          ],
        }),
      ],
      comments: [],
      versions: [],
    });
    const commit = events.find((e) => e.kind === "commit");
    expect(commit && commit.kind === "commit" && commit.subject).toBeUndefined();
    expect(commit && commit.kind === "commit" && commit.sha).toBe("def5678");
  });

  it("falls back to a sha_full prefix when the short sha is absent", () => {
    const events = assembleBiography({
      sessions: [
        session({
          commits: [
            {
              kind: "commit",
              sha_full: "0123456789abcdef",
              resolved: false,
              trailers: [],
            },
          ],
        }),
      ],
      comments: [],
      versions: [],
    });
    const commit = events.find((e) => e.kind === "commit");
    expect(commit && commit.kind === "commit" && commit.sha).toBe("0123456789");
  });

  it("collapses the comment thread into one dated beat, not a row per comment", () => {
    const events = assembleBiography({
      sessions: [],
      comments: [
        comment({ id: "c1", status: "open", createdAt: new Date(1000 * 1000).toISOString() }),
        comment({ id: "c2", status: "resolved", createdAt: new Date(3000 * 1000).toISOString() }),
        comment({ id: "c3", status: "resolved", createdAt: new Date(2000 * 1000).toISOString() }),
      ],
      versions: [],
    });
    expect(events).toHaveLength(1);
    const beat = events[0];
    expect(beat.kind).toBe("comments");
    expect(beat.kind === "comments" && beat.openCount).toBe(1);
    expect(beat.kind === "comments" && beat.resolvedCount).toBe(2);
    expect(beat.unix).toBe(3000);
  });

  it("omits the comments beat entirely when there are no comments", () => {
    const events = assembleBiography({ sessions: [], comments: [], versions: [] });
    expect(events.find((e) => e.kind === "comments")).toBeUndefined();
  });

  it("keeps construction order as the stable tie-break for equal unix", () => {
    // Two sessions land at the exact same instant — the one submitted first
    // (already newest-first from the server) must stay first.
    const events = assembleBiography({
      sessions: [
        session({ session_id: "first", started_at: 42 }),
        session({ session_id: "second", started_at: 42 }),
      ],
      comments: [],
      versions: [],
    });
    expect(
      events.map((e) => (e.kind === "session" || e.kind === "origin" ? e.sessionId : "")),
    ).toEqual(["first", "second"]);
  });
});

describe("splitBiography", () => {
  function fill(n: number): BioEvent[] {
    return Array.from({ length: n }, (_, i) => ({
      kind: "version",
      id: `v${i}`,
      unix: n - i,
      ref: `r${i}`,
      label: "",
      short: `r${i}`,
      source: "index",
    }));
  }

  it("collapses nothing when at or under the cap", () => {
    const events = fill(BIOGRAPHY_VISIBLE_CAP);
    const { visible, collapsed } = splitBiography(events);
    expect(visible).toHaveLength(BIOGRAPHY_VISIBLE_CAP);
    expect(collapsed).toHaveLength(0);
  });

  it("collapses everything past the cap", () => {
    const events = fill(BIOGRAPHY_VISIBLE_CAP + 5);
    const { visible, collapsed } = splitBiography(events);
    expect(visible).toHaveLength(BIOGRAPHY_VISIBLE_CAP);
    expect(collapsed).toHaveLength(5);
    expect(collapsed[0].id).toBe(`v${BIOGRAPHY_VISIBLE_CAP}`);
  });

  it("honors a custom cap", () => {
    const events = fill(10);
    const { visible, collapsed } = splitBiography(events, 3);
    expect(visible).toHaveLength(3);
    expect(collapsed).toHaveLength(7);
  });
});
