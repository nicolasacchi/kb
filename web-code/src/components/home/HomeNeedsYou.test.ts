import { describe, expect, it } from "vitest";
import type { KbBlockedOut } from "../../api/types";
import { blockedLane, needsYouBadge, nonEmptyLanes } from "./HomeNeedsYou";

describe("nonEmptyLanes (OL-C7)", () => {
  it("is empty when every lane is zero or absent", () => {
    expect(nonEmptyLanes(0, 0, null, null)).toEqual([]);
    expect(nonEmptyLanes(0, 0, 0, 0)).toEqual([]);
  });

  it("keeps only lanes with a count, in display order, with singular labels", () => {
    const rows = nonEmptyLanes(1, 0, 3, null);
    expect(rows.map((r) => r.lane)).toEqual(["reviews", "kb-desk"]);
    expect(rows[0].label).toBe("1 review awaiting you");
    expect(rows[1].label).toBe("3 on your kb desk");
  });
});

function blockedOut(over: Partial<KbBlockedOut> = {}): KbBlockedOut {
  return {
    sessions: [
      { session_id: "a", harness: "claude", project: "kb", blocked_tool: "Bash", since_unix: 100 },
    ],
    kb_public_url: "https://kb.example.com/",
    total: 1,
    truncated: false,
    ...over,
  };
}

describe("blockedLane (OL IDEA1)", () => {
  it("is null when the relay is absent, unavailable or empty", () => {
    expect(blockedLane(undefined)).toBeNull();
    expect(blockedLane(blockedOut({ unavailable: "kb unreachable", sessions: [], total: 0 }))).toBeNull();
    expect(blockedLane(blockedOut({ sessions: [], total: 0 }))).toBeNull();
  });

  it("labels one session with project and tool, falling back to the harness", () => {
    expect(blockedLane(blockedOut())?.label).toBe(
      "1 session blocked on you \u00b7 kb \u00b7 permission: Bash",
    );
    const bare = blockedOut({
      sessions: [{ session_id: "a", harness: "codex", since_unix: 1 }],
    });
    expect(blockedLane(bare)?.label).toBe("1 session blocked on you \u00b7 codex");
  });

  it("labels several sessions by count, using the total", () => {
    const out = blockedOut({
      sessions: [
        { session_id: "a", harness: "claude", since_unix: 1 },
        { session_id: "b", harness: "claude", since_unix: 2 },
      ],
      total: 3,
    });
    const row = blockedLane(out);
    expect(row?.label).toBe("3 sessions blocked on you");
    expect(row?.count).toBe(3);
  });

  it("builds the href from the public url, trimming trailing slashes", () => {
    expect(blockedLane(blockedOut())?.href).toBe("https://kb.example.com/sessions#kb-now-blocked");
    expect(blockedLane(blockedOut({ kb_public_url: "https://kb.example.com///" }))?.href).toBe(
      "https://kb.example.com/sessions#kb-now-blocked",
    );
  });

  it("has no href without a public url", () => {
    expect(blockedLane(blockedOut({ kb_public_url: "" }))?.href).toBeUndefined();
  });

  it("goes first in nonEmptyLanes and existing 4-arg calls are unchanged", () => {
    const row = blockedLane(blockedOut());
    const rows = nonEmptyLanes(1, 0, 3, null, row);
    expect(rows.map((r) => r.lane)).toEqual(["kb-blocked", "reviews", "kb-desk"]);
    expect(nonEmptyLanes(1, 0, 3, null).map((r) => r.lane)).toEqual(["reviews", "kb-desk"]);
    expect(nonEmptyLanes(0, 0, null, null, row).map((r) => r.lane)).toEqual(["kb-blocked"]);
  });

  it("the section badge is the inbox badge plus the blocked count", () => {
    expect(needsYouBadge(4, 2)).toBe(6);
    expect(needsYouBadge(null, 2)).toBe(2);
    expect(needsYouBadge(null, 0)).toBe(0);
  });
});
