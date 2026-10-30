import { describe, expect, it } from "vitest";
import type { ChangesProject, ChangesResponse, ChangesSession } from "../api/changes";
import {
  ambientSinceHref,
  composeSinceStrip,
  countsLine,
  projectHref,
  sinceHeading,
} from "./sinceYouLeft";

function session(over: Partial<ChangesSession> = {}): ChangesSession {
  return {
    kb: "sessions",
    session_id: "sid-1",
    display_name: "wire the route",
    harness: "claude",
    started_at: 100,
    ended_at: 200,
    started_before_since: false,
    commits: [],
    commits_total: 0,
    ...over,
  };
}

function project(over: Partial<ChangesProject> = {}): ChangesProject {
  return {
    project: "kb",
    label: "kb",
    source: "registry",
    latest_unix: 200,
    sessions: [],
    sessions_total: 0,
    sessions_truncated: false,
    commit_count: 0,
    ...over,
  };
}

function resp(projects: ChangesProject[], total = projects.length): ChangesResponse {
  return {
    since_unix: 1000,
    to_unix: 2000,
    window_clamped: false,
    projects,
    projects_total: total,
    projects_truncated: total > projects.length,
    sessions_scan_capped: false,
    trivial_sessions_hidden: 0,
  };
}

describe("sinceYouLeft", () => {
  it("heading_formats_duration_and_clamp", () => {
    const now = 1_000_000 * 1000;
    expect(sinceHeading(1_000_000 - (2 * 3600 + 14 * 60), now, false)).toBe(
      "Since you left · 2h 14m",
    );
    expect(sinceHeading(1_000_000 - 30 * 60, now, false)).toBe("Since you left · 30m");
    expect(sinceHeading(1_000_000 - 3 * 86_400, now, false)).toBe("Since you left · 3d");
    expect(sinceHeading(1_000_000 - 3600, now, true)).toBe(
      "Since you left · over 31 days (showing the last 31)",
    );
  });

  it("rows_cap_at_six_with_more_count", () => {
    const ps = Array.from({ length: 8 }, (_, i) => project({ project: `p${i}`, label: `p${i}` }));
    const strip = composeSinceStrip(resp(ps, 23));
    expect(strip.rows).toHaveLength(6);
    // The server's own total, not the length of what it sent.
    expect(strip.more).toBe(23 - 6);
    expect(composeSinceStrip(resp(ps.slice(0, 3))).more).toBe(0);
  });

  it("details_priority_attention_then_commits_then_sessions", () => {
    const p = project({
      sessions: [
        session({
          closing: "landed",
          commits: [
            { kind: "commit", sha: "abcdef0123", subject: "feat: route" },
            { kind: "push", sha: "abcdef0123", subject: "feat: route" },
          ],
        }),
      ],
      sessions_total: 1,
      slate: {
        slug: "kb",
        head_seq: 3,
        closed: false,
        posts: [
          { seq: 3, kind: "found", line: "a finding", at: 1, who: "x", closed: false },
          { seq: 2, kind: "hand", line: "closed hand", at: 1, who: "x", closed: true },
          { seq: 1, kind: "ask", line: "open question", at: 1, who: "x", closed: false },
        ],
        posts_total: 3,
        posts_truncated: false,
        other_total: 0,
        retracted_total: 0,
        archived_in_window: false,
      },
    });
    // Room for everything: attention, then the commit, then the session.
    const all = composeSinceStrip(resp([p]), { maxDetails: 5 }).rows[0].details;
    expect(all.map((d) => d.kind)).toEqual(["attention", "commit", "session"]);
    expect(all[0].text).toBe("ask: open question");
    expect(all[1].text).toBe("abcdef0 feat: route");
    expect(all[2].text).toBe("wire the route — landed");
    // The default of two keeps the first two priorities.
    const two = composeSinceStrip(resp([p])).rows[0].details;
    expect(two.map((d) => d.kind)).toEqual(["attention", "commit"]);
    // Without attention the commit leads.
    const noSlate = composeSinceStrip(resp([{ ...p, slate: undefined }])).rows[0].details;
    expect(noSlate.map((d) => d.kind)).toEqual(["commit", "session"]);
  });

  it("counts_use_server_totals_only", () => {
    const p = project({
      sessions: [session()],
      sessions_total: 9,
      commit_count: 7,
      slate: {
        slug: "kb",
        head_seq: 5,
        closed: false,
        posts: [],
        posts_total: 1,
        posts_truncated: false,
        other_total: 1,
        retracted_total: 0,
        archived_in_window: false,
      },
    });
    expect(countsLine(p)).toBe("9 sessions · 7 commits · slate 2 new");
    expect(countsLine(project({ sessions_total: 1, commit_count: 1 }))).toBe(
      "1 session · 1 commit",
    );
    expect(countsLine(project())).toBe("");
  });

  it("links_target_sessions_project_or_slate", () => {
    expect(projectHref({ project: "kb", source: "registry" })).toBe("/sessions?project=kb");
    expect(projectHref({ project: "-home-x-y", source: "derived" })).toBe(
      "/sessions?project=-home-x-y",
    );
    expect(projectHref({ project: "orchard", source: "slate" })).toBe("/slates/orchard");
    expect(projectHref({ project: "", source: "derived" })).toBe("/sessions");
    const d = composeSinceStrip(
      resp([project({ sessions: [session({ kb: "s", session_id: "abc" })], sessions_total: 1 })]),
    ).rows[0].details[0];
    expect(d.href).toBe("/sessions?kb=s&focus=abc");
  });

  it("ambient footer link carries since and optionally the kb", () => {
    expect(ambientSinceHref(1000, null)).toBe("/ambient?since=1000");
    expect(ambientSinceHref(1000, "canon")).toBe("/ambient?since=1000&kb=canon");
  });
});
