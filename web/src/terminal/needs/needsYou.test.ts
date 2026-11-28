import { describe, expect, it } from "vitest";
import type { ConversationResult, LiveStatusRow } from "../../api/sessions";
import { captureMtime, deriveNeeds, GENERIC_NEEDS_TEXT } from "./needsYou";

const SINCE = 1_700_000_000;

function live(over: Partial<LiveStatusRow> = {}): LiveStatusRow {
  return { session_id: "sid-1", blocked: true, blocked_since_unix: SINCE, ...over } as LiveStatusRow;
}

function conv(
  gen: string,
  ask: { kind?: string; q: string; options?: string[]; pending?: boolean } | null,
  earlier: { q: string } | null = null,
): ConversationResult {
  const mk = (n: number, a: { kind?: string; q: string; options?: string[]; pending?: boolean } | null) => ({
    n,
    id: `t-${n}`,
    ts: null,
    prompt: "p",
    answer: null,
    asks: a ? [{ kind: a.kind ?? "ask", q: a.q, options: a.options ?? [], answer: null, pending: a.pending ?? true }] : [],
    commits: [],
    files_touched: 0,
    errors: 0,
    subagents: { n: 0, errors: 0 },
  });
  const rows = [...(earlier ? [mk(1, { q: earlier.q })] : []), mk(2, ask)];
  return {
    kind: "ok",
    kb: "demo",
    conversation: { schema: "conversation/1", total: rows.length, from: 1, next_cursor: null, capture_gen: gen, unchanged: false, rows },
  };
}

describe("deriveNeeds", () => {
  it("is null unless the row is blocked", () => {
    expect(deriveNeeds(undefined, undefined)).toBeNull();
    expect(deriveNeeds(live({ blocked: false }), conv(`${SINCE + 5}-2`, { q: "Q?" }))).toBeNull();
  });

  it("takes the pending ask of a capture at least as new as the block", () => {
    const n = deriveNeeds(live(), conv(`${SINCE}-2`, { q: "Which db?", options: ["pg", "sqlite"] }));
    expect(n).toMatchObject({ kind: "question", text: "Which db?", options: ["pg", "sqlite"], source: "capture", sinceUnix: SINCE });
  });

  it("a plan ask is kind plan", () => {
    expect(deriveNeeds(live(), conv(`${SINCE + 1}-2`, { kind: "plan", q: "Approve plan?" }))?.kind).toBe("plan");
  });

  it("only the last row's pending ask counts", () => {
    const c = conv(`${SINCE + 1}-2`, null, { q: "old question" });
    const n = deriveNeeds(live({ detail_reason: "Claude needs permission" }), c);
    expect(n?.source).toBe("beat");
    expect(n?.text).toBe("Claude needs permission");
  });

  it("a capture OLDER than the block never supplies the question", () => {
    const n = deriveNeeds(live({ detail_reason: "beat text" }), conv(`${SINCE - 1}-2`, { q: "stale question" }));
    expect(n).toMatchObject({ source: "beat", text: "beat text", kind: "permission" });
  });

  it("missing blocked_since_unix or a bad capture_gen is never trusted", () => {
    const noSince = live({ detail_reason: "r" });
    delete (noSince as { blocked_since_unix?: number }).blocked_since_unix;
    expect(deriveNeeds(noSince, conv(`${SINCE + 9}-2`, { q: "Q" }))?.source).toBe("beat");
    expect(deriveNeeds(live({ detail_reason: "r" }), conv("garbage", { q: "Q" }))?.source).toBe("beat");
    expect(captureMtime("12-3")).toBe(12);
    expect(captureMtime("12")).toBeNull();
    expect(captureMtime("-3")).toBeNull();
  });

  it("a blocked tool alone is a permission with the generic text", () => {
    const n = deriveNeeds(live({ blocked_tool: "Bash" }), undefined);
    expect(n).toMatchObject({ kind: "permission", tool: "Bash", text: GENERIC_NEEDS_TEXT, source: "none" });
  });

  it("nothing known falls to the generic attention text", () => {
    expect(deriveNeeds(live(), { kind: "none" })).toMatchObject({ kind: "attention", text: "Waiting for you", source: "none" });
  });

  it("last_line is secondary and never becomes the text", () => {
    const n = deriveNeeds(live({ last_line: "ran the tests" }), undefined);
    expect(n?.text).toBe(GENERIC_NEEDS_TEXT);
    expect(n?.lastLine).toBe("ran the tests");
    const c = deriveNeeds(live({ last_line: "ran the tests" }), conv(`${SINCE}-2`, { q: "Q?" }));
    expect(c?.lastLine).toBeUndefined();
  });

  it("markup in the source stays a plain string", () => {
    const evil = "<img src=x onerror=alert(1)>";
    const n = deriveNeeds(live(), conv(`${SINCE}-2`, { q: evil }));
    expect(n?.text).toBe(evil);
  });
});
