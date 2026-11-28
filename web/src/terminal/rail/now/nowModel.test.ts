import { describe, expect, it } from "vitest";
import type { LiveStatusRow } from "../../../api/generated/LiveStatusRow";
import type { Conversation } from "../../../api/generated/Conversation";
import type { SessionStats } from "../../../api/generated/SessionStats";
import type { SessionCard } from "../../../api/sessions";
import type { TermRow } from "../../../api/terms";
import { card as slateCard } from "../board/boardRows.testkit";
import { NOW_SECTION_ORDER, buildNow, captureMtime, deriveStats, nowBadge, type NowInput, type NowSection } from "./nowModel";

const NOW = 1_700_000_000;
const SID = "sid-1";

const row = (over: Partial<LiveStatusRow> = {}): LiveStatusRow =>
  ({
    session_id: SID,
    harness: "claude",
    holder: "agent",
    state: "working",
    source: "hook",
    confidence: "observed",
    since_unix: NOW - 100,
    since_secs: 100,
    resume: "claude -r x",
    blocked: false,
    ...over,
  }) as LiveStatusRow;

const timeStats = {
  wall_secs: 3600,
  active_secs: 1200,
  turns_timed: 3,
  turn_mean_secs: 60,
  turn_median_secs: 50,
  turn_max_secs: 120,
  turn_source: "harness_reported",
  wait_secs: 300,
  wait_gaps: 2,
  wait_median_secs: 100,
  wait_gaps_excluded: 1,
  pending_ask: false,
  clock_regressions: 0,
} as const;

const stats = (over: Partial<SessionStats> = {}): SessionStats => ({
  input_tokens: 1234,
  output_tokens: 5678,
  cache_read_tokens: 9000,
  cache_write_tokens: 100,
  cache_hit_ratio: 0.9,
  turns: 3,
  tool_calls: 12,
  assistant_messages: 6,
  usage_conflicts: 0,
  usage_schema_unknown: 0,
  context_tokens: 100_000,
  cost_usd: null,
  time: { ...timeStats },
  ...over,
});

const card = (over: Record<string, unknown> = {}): SessionCard =>
  ({
    schema: "kb-session-card/1",
    session_id: SID,
    outcome: { available: true, last_assistant_text: "All done." },
    children: { available: true, total: 0 },
    stats: { available: true, model: "claude-opus-4-8", stats: stats() },
    ...over,
  }) as unknown as SessionCard;

const conv = (gen: string, pending = true, kind = "ask"): Conversation => ({
  schema: "x",
  total: 1,
  from: 0,
  next_cursor: null,
  capture_gen: gen,
  unchanged: false,
  rows: [
    {
      n: 1,
      id: "t1",
      ts: null,
      prompt: "p",
      answer: null,
      asks: [{ kind, q: "Which database?", options: [], answer: null, pending }],
      commits: [],
      files_touched: 0,
      errors: 0,
      subagents: { total: 0 } as never,
    },
  ],
});

const term = (id: string, over: Partial<TermRow> = {}): TermRow =>
  ({
    id,
    harness: "claude",
    cwd: "/work/proj",
    started_unix: NOW - 500,
    resume_sid: null,
    pane_pid: 1,
    cols: 80,
    rows: 24,
    bracket_paste_flag: false,
    pane_dead: false,
    exit_code: null,
    grid_owner: null,
    conf_version: 1,
    anchor_pid: null,
    stale: false,
    session_id: null,
    ...over,
  }) as TermRow;

const base = (over: Partial<NowInput> = {}): NowInput => ({ termId: "t1", sid: SID, row: row(), card: card(), terms: [term("t1")], nowUnix: NOW, ...over });
const sec = <K extends NowSection["id"]>(m: ReturnType<typeof buildNow>, id: K): Extract<NowSection, { id: K }> | undefined =>
  m.sections.find((s) => s.id === id) as Extract<NowSection, { id: K }> | undefined;

describe("buildNow sections", () => {
  it("emits every section in the fixed order with a freshness label", () => {
    const m = buildNow(base({ row: row({ blocked: true, blocked_since_unix: NOW - 60 }), asks: [] }));
    expect(m.sections.map((s) => s.id)).toEqual([...NOW_SECTION_ORDER]);
    for (const s of m.sections.filter((x) => x.id !== "queue")) expect(s.fresh).not.toBe("");
  });

  it("omits needs-you when the session is not blocked", () => {
    const m = buildNow(base());
    expect(sec(m, "needs")).toBeUndefined();
    expect(m.sections.map((s) => s.id)).not.toContain("needs");
  });

  it("an unbound terminal derives only siblings and the queue seam", () => {
    const m = buildNow(base({ sid: undefined, row: null, card: null }));
    expect(m.linked).toBe(false);
    expect(m.sections.map((s) => s.id)).toEqual(["siblings", "queue"]);
  });
});

describe("needs-you", () => {
  const blocked = row({ blocked: true, blocked_since_unix: NOW - 120, detail_reason: "Needs a go-ahead", last_line: "last words" });

  it("takes the pending question only from a capture at least as new as the block", () => {
    const fresh = buildNow(base({ row: blocked, conv: conv(`${NOW - 60}-4`) }));
    const n = sec(fresh, "needs")!.needs;
    expect(n.kind).toBe("question");
    expect(n.text).toBe("Which database?");
    expect(n.textSource).toBe("question");
    expect(n.since).toBe("2m ago");
    expect(n.lastLine).toBe("last words");
    const exact = buildNow(base({ row: blocked, conv: conv(`${NOW - 120}-4`) }));
    expect(sec(exact, "needs")!.needs.textSource).toBe("question");
  });

  it("rejects a stale capture and falls back to the live detail reason", () => {
    const stale = buildNow(base({ row: blocked, conv: conv(`${NOW - 300}-4`) }));
    const n = sec(stale, "needs")!.needs;
    expect(n.textSource).toBe("reason");
    expect(n.text).toBe("Needs a go-ahead");
    expect(n.kind).toBe("waiting");
  });

  it("rejects an unparsable capture_gen and an unknown block time", () => {
    expect(captureMtime("nope")).toBeNull();
    expect(captureMtime("17-3")).toBe(17);
    const bad = buildNow(base({ row: blocked, conv: conv("garbage") }));
    expect(sec(bad, "needs")!.needs.textSource).toBe("reason");
    const noSince = buildNow(base({ row: row({ blocked: true }), conv: conv(`${NOW}-4`) }));
    expect(sec(noSince, "needs")!.needs.textSource).toBe("default");
    expect(sec(noSince, "needs")!.needs.text).toBe("Waiting for you");
  });

  it("ignores an answered ask and the last_line is never the question", () => {
    const m = buildNow(base({ row: row({ blocked: true, blocked_since_unix: NOW - 10, last_line: "SECRET LINE" }), conv: conv(`${NOW}-1`, false) }));
    const n = sec(m, "needs")!.needs;
    expect(n.text).toBe("Waiting for you");
    expect(n.text).not.toContain("SECRET");
    expect(n.lastLine).toBe("SECRET LINE");
  });

  it("a permission prompt names the tool and plan asks are labelled plan", () => {
    const perm = buildNow(base({ row: row({ blocked: true, blocked_tool: "Bash", blocked_since_unix: NOW - 5 }) }));
    expect(sec(perm, "needs")!.needs).toMatchObject({ kind: "permission", tool: "Bash" });
    const plan = buildNow(base({ row: blocked, conv: conv(`${NOW}-1`, true, "plan") }));
    expect(sec(plan, "needs")!.needs.kind).toBe("plan");
  });
});

describe("activity", () => {
  it("omits absent fields and never invents zeros", () => {
    const a = sec(buildNow(base()), "activity")!.activity;
    expect(a).toEqual({ lastTool: null, background: null, subagents: null });
  });

  it("carries last tool (started), background counts and subagents", () => {
    const m = buildNow(
      base({
        row: row({ last_tool: "Edit", last_tool_at: NOW - 30, background: { agent: 1, shell: 0, other: 2 } }),
        card: card({ children: { available: true, total: 4 } }),
      }),
    );
    expect(sec(m, "activity")!.activity).toEqual({
      lastTool: { name: "Edit", since: "just now" },
      background: { agent: 1, shell: 0, other: 2 },
      subagents: 4,
    });
  });
});

describe("answer", () => {
  it("is the card outcome, clamped, and absent when the lane is unavailable", () => {
    const long = "x".repeat(2000);
    const m = buildNow(base({ card: card({ outcome: { available: true, last_assistant_text: long } }) }));
    expect(sec(m, "answer")!.text.length).toBeLessThan(700);
    const none = buildNow(base({ card: card({ outcome: { available: false, reason: "no capture yet" } }) }));
    expect(sec(none, "answer")).toBeUndefined();
  });
});

describe("siblings", () => {
  const terms = [
    term("t1", { cwd: "/a/proj" }),
    term("t2", { cwd: "/b/proj", harness: "omp", state: "working" }),
    term("t3", { cwd: "/c/other" }),
    term("t4", { cwd: "/d/proj", pane_dead: true }),
  ];
  it("excludes self, ended terminals and other projects", () => {
    const m = buildNow(base({ terms, marks: { t2: { asks: true, finishedUnseen: false } } }));
    const s = sec(m, "siblings")!.siblings;
    expect(s.map((x) => x.id)).toEqual(["t2"]);
    expect(s[0]).toMatchObject({ harness: "omp", mark: "asks", state: "working" });
  });
  it("is dropped when the terminal list could not be read", () => {
    expect(sec(buildNow(base({ terms, termsOk: false })), "siblings")).toBeUndefined();
  });
});

describe("stats", () => {
  it("shows n/a for null figures instead of zeros", () => {
    const s = deriveStats(
      card({
        stats: {
          available: true,
          stats: stats({ cache_hit_ratio: null, context_tokens: null, time: { ...timeStats, wall_secs: null, active_secs: null, turn_mean_secs: null, turn_median_secs: null, turn_max_secs: null, turn_source: null, wait_secs: null } }),
        },
      }),
      null,
    );
    const by = (id: string) => s.rows.find((r) => r.id === id)!.value;
    expect(by("cache")).toContain("hit n/a");
    expect(by("time")).toBe("n/a / n/a");
    expect(by("turn")).toBe("n/a / n/a / n/a");
    expect(by("wait")).toContain("n/a in");
    expect(by("context")).toBe("n/a");
  });

  it("omits cost unless the harness reported one", () => {
    expect(deriveStats(card(), row()).rows.find((r) => r.id === "cost")).toBeUndefined();
    const fromCapture = deriveStats(card({ stats: { available: true, model: "claude-opus-4-8", stats: stats({ cost_usd: 1.234 }) } }), row());
    expect(fromCapture.rows.find((r) => r.id === "cost")!.value).toBe("$1.23");
    const live = deriveStats(card(), row({ status: { cost_usd: 0.5 } }));
    expect(live.rows.find((r) => r.id === "cost")).toMatchObject({ value: "$0.50", source: "harness-reported, live" });
  });

  it("computes a context percentage only with a known window, preferring the live figure", () => {
    const known = deriveStats(card(), row());
    expect(known.rows.find((r) => r.id === "context")!.value).toBe("50% (100,000 tokens)");
    const unknown = deriveStats(card({ stats: { available: true, model: "mystery", stats: stats() } }), row());
    expect(unknown.rows.find((r) => r.id === "context")!.value).toBe("100,000 tokens");
    const live = deriveStats(card(), row({ status: { context_pct: 41.6 } }));
    expect(live.rows.find((r) => r.id === "context")).toMatchObject({ value: "42%", source: "harness-reported, live" });
  });

  it("an unavailable lane carries its reason and no rows", () => {
    const s = deriveStats(card({ stats: { available: false, reason: "capture unreadable" } }), null);
    expect(s).toEqual({ available: false, reason: "capture unreadable", rows: [] });
  });
});

describe("asks and badge", () => {
  it("lists only the asks it is given and drops the section without a board", () => {
    expect(sec(buildNow(base({ asks: null })), "asks")).toBeUndefined();
    const m = buildNow(base({ asks: [slateCard(4, "ask", { to: "you" })] }));
    expect(sec(m, "asks")!.open).toHaveLength(1);
  });
  it("badges 1 only for a blocked row", () => {
    expect(nowBadge(row({ blocked: true }))).toBe(1);
    expect(nowBadge(row())).toBeUndefined();
    expect(nowBadge(null)).toBeUndefined();
  });
});
