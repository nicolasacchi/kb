// RAILNOW — the pure model of the Now tab. No React, no clock (the caller
// passes `nowUnix`), no fetching: every section is derived from data the
// terminal already holds (live-status row, session card, conversation
// projection, slate asks, terminal list and attention marks) and says how
// fresh it is. Nothing here reads terminal bytes, scores or ranks anything.

import type { LiveStatusRow } from "../../../api/generated/LiveStatusRow";
import type { Conversation } from "../../../api/generated/Conversation";
import type { ConvAsk } from "../../../api/generated/ConvAsk";
import type { SessionCard } from "../../../api/sessions";
import type { SlateBoardCard } from "../../../api/slateTypes";
import type { TermRow } from "../../../api/terms";
import { humanizeDuration, relativeAge } from "../../../lib/time";
import { joinHub } from "../../hub/termHub";
import { statusChips, type StatusChip } from "../../statusChips";
import type { TermMarks } from "../../notify/attention";
import { contextPercent } from "./contextWindow";

export const NOW_SECTION_ORDER = ["needs", "activity", "status", "answer", "asks", "siblings", "stats", "queue"] as const;
export type NowSectionId = (typeof NOW_SECTION_ORDER)[number];

export const LIVE_FRESH = "live, polled 10 s";
export const CAPTURE_FRESH = "as of last capture";
export const SLATE_FRESH = "as of slate update";
export const TERMS_FRESH = "live, updates on change";
const ANSWER_CLAMP = 600;
const SIBLING_CAP = 8;

export type NeedsKind = "permission" | "question" | "plan" | "waiting";

export interface NeedsView {
  kind: NeedsKind;
  /** The tool a permission prompt names. */
  tool: string | null;
  /** "just now" / "4m ago" since the block began, or null when unknown. */
  since: string | null;
  /** The question text, else the live detail reason, else "Waiting for you". */
  text: string;
  textSource: "question" | "reason" | "default";
  /** Muted secondary line; never the question. */
  lastLine: string | null;
}

export interface ActivityView {
  lastTool: { name: string; since: string | null } | null;
  background: { agent: number; shell: number; other: number } | null;
  subagents: number | null;
}

export interface SiblingView {
  id: string;
  harness: string;
  title: string;
  project: string;
  state: string;
  blocked: boolean;
  mark: "asks" | "finished" | null;
}

export interface StatRow {
  id: "tokens" | "cache" | "turns" | "tools" | "time" | "turn" | "wait" | "context" | "cost";
  label: string;
  value: string;
  /** Where the figure came from, when it matters (context, cost). */
  source?: string;
}

export interface StatsView {
  available: boolean;
  reason: string | null;
  rows: StatRow[];
}

interface Base<K extends NowSectionId> {
  id: K;
  title: string;
  fresh: string;
}
export type NowSection =
  | (Base<"needs"> & { needs: NeedsView })
  | (Base<"activity"> & { activity: ActivityView })
  | (Base<"status"> & { chips: StatusChip[] })
  | (Base<"answer"> & { text: string })
  | (Base<"asks"> & { open: SlateBoardCard[] })
  | (Base<"siblings"> & { siblings: SiblingView[] })
  | (Base<"stats"> & { stats: StatsView })
  | Base<"queue">;

export interface NowModel {
  /** False while the terminal has no bound session: sections 1-7 are not derived. */
  linked: boolean;
  sections: NowSection[];
}

export interface NowInput {
  /** This terminal's id (siblings exclude it). */
  termId: string;
  /** The bound session id, when linked. */
  sid?: string;
  /** The live-status row of the bound session, when the registry has one. */
  row?: LiveStatusRow | null;
  card?: SessionCard | null;
  /** The conversation projection; only consulted while the row is blocked. */
  conv?: Conversation | null;
  /** Open slate asks addressed to the operator; null = no board available. */
  asks?: readonly SlateBoardCard[] | null;
  terms?: readonly TermRow[];
  /** False when the terminal list could not be read: the siblings section is dropped. */
  termsOk?: boolean;
  live?: readonly LiveStatusRow[];
  marks?: TermMarks;
  nowUnix: number;
}

/** `capture_gen` is `<mtime_unix>-<turns>`; null when it is not that shape. */
export function captureMtime(gen: string | null | undefined): number | null {
  const m = typeof gen === "string" ? /^(\d+)-\d+$/.exec(gen) : null;
  return m ? Number(m[1]) : null;
}

function ageText(unix: number | null | undefined, nowUnix: number): string | null {
  if (typeof unix !== "number" || !Number.isFinite(unix) || unix <= 0) return null;
  const a = relativeAge(unix, nowUnix * 1000);
  return a === "now" ? "just now" : `${a} ago`;
}

/**
 * The pending question, only when the conversation capture is at least as new
 * as the block: a capture older than `blocked_since_unix` cannot know the
 * question that raised it (it would be a stale one). An unparsable
 * `capture_gen` or an unknown block time rejects.
 */
export function pendingAsk(conv: Conversation | null | undefined, blockedSince: number | undefined): ConvAsk | null {
  if (!conv || typeof blockedSince !== "number") return null;
  const mtime = captureMtime(conv.capture_gen);
  if (mtime === null || mtime < blockedSince) return null;
  const last = conv.rows[conv.rows.length - 1];
  return last?.asks.find((a) => a.pending) ?? null;
}

function deriveNeeds(row: LiveStatusRow, conv: Conversation | null | undefined, nowUnix: number): NeedsView {
  const ask = row.blocked_tool ? null : pendingAsk(conv, row.blocked_since_unix);
  const kind: NeedsKind = row.blocked_tool ? "permission" : ask ? (ask.kind === "plan" ? "plan" : "question") : "waiting";
  const q = ask && ask.q.trim() !== "" ? ask.q : null;
  const reason = row.detail_reason && row.detail_reason.trim() !== "" ? row.detail_reason : null;
  return {
    kind,
    tool: row.blocked_tool ?? null,
    since: ageText(row.blocked_since_unix, nowUnix),
    text: q ?? reason ?? "Waiting for you",
    textSource: q ? "question" : reason ? "reason" : "default",
    lastLine: row.last_line && row.last_line.trim() !== "" ? row.last_line : null,
  };
}

function deriveActivity(row: LiveStatusRow | null | undefined, card: SessionCard | null | undefined, nowUnix: number): ActivityView {
  const bg = row?.background;
  const total = card?.children?.available ? card.children.total : 0;
  return {
    lastTool: row?.last_tool ? { name: row.last_tool, since: ageText(row.last_tool_at, nowUnix) } : null,
    background: bg && bg.agent + bg.shell + bg.other > 0 ? { agent: bg.agent, shell: bg.shell, other: bg.other } : null,
    subagents: total > 0 ? total : null,
  };
}

const NA = "n/a";

function int(n: number | null | undefined): string {
  if (typeof n !== "number" || !Number.isFinite(n)) return NA;
  return String(Math.round(n)).replace(/\B(?=(\d{3})+(?!\d))/g, ",");
}

function secs(n: number | null | undefined): string {
  if (typeof n !== "number" || !Number.isFinite(n)) return NA;
  return n < 1 ? "0s" : humanizeDuration(n * 1000);
}

function pct(r: number | null | undefined): string {
  return typeof r === "number" && Number.isFinite(r) ? `${Math.round(r * 100)}%` : NA;
}

const SOURCE_CAPTION: Record<string, string> = {
  timestamps: "from record timestamps",
  harness_reported: "harness-reported turn times",
  mixed: "mixed sources",
};

export function deriveStats(card: SessionCard | null | undefined, row: LiveStatusRow | null | undefined): StatsView {
  const lane = card?.stats;
  const rows: StatRow[] = [];
  const s = lane?.available ? lane.stats : undefined;
  if (s) {
    const t = s.time;
    rows.push(
      { id: "tokens", label: "Tokens in / out", value: `${int(s.input_tokens)} / ${int(s.output_tokens)}` },
      { id: "cache", label: "Cache read / write", value: `${int(s.cache_read_tokens)} / ${int(s.cache_write_tokens)} (hit ${pct(s.cache_hit_ratio)})` },
      { id: "turns", label: "Turns", value: int(s.turns) },
      { id: "tools", label: "Tool calls", value: int(s.tool_calls) },
      { id: "time", label: "Active / wall", value: `${secs(t.active_secs)} / ${secs(t.wall_secs)}` },
      {
        id: "turn",
        label: "Per turn mean / median / max",
        value: `${secs(t.turn_mean_secs)} / ${secs(t.turn_median_secs)} / ${secs(t.turn_max_secs)}`,
        source: t.turn_source ? (SOURCE_CAPTION[t.turn_source] ?? t.turn_source) : undefined,
      },
      {
        id: "wait",
        label: "Operator wait",
        value: `${secs(t.wait_secs)} in ${int(t.wait_gaps)} gaps (${int(t.wait_gaps_excluded)} excluded)`,
      },
    );
  }
  // Context: the harness-reported live percentage wins over the computed one.
  const live = row?.status?.context_pct;
  const model = lane?.model ?? row?.status?.model;
  if (typeof live === "number" && Number.isFinite(live)) {
    rows.push({ id: "context", label: "Context", value: `${Math.round(live)}%`, source: "harness-reported, live" });
  } else if (s) {
    const p = contextPercent(s.context_tokens, model);
    if (p !== null) {
      rows.push({ id: "context", label: "Context", value: `${p}% (${int(s.context_tokens)} tokens)`, source: "computed from the last capture" });
    } else {
      rows.push({
        id: "context",
        label: "Context",
        value: s.context_tokens == null ? NA : `${int(s.context_tokens)} tokens`,
        source: s.context_tokens == null ? undefined : "window unknown for this model",
      });
    }
  }
  // Cost only when the harness reported one; never estimated.
  const liveCost = row?.status?.cost_usd;
  if (typeof liveCost === "number" && Number.isFinite(liveCost)) {
    rows.push({ id: "cost", label: "Cost", value: `$${liveCost.toFixed(2)}`, source: "harness-reported, live" });
  } else if (s && typeof s.cost_usd === "number" && Number.isFinite(s.cost_usd)) {
    rows.push({ id: "cost", label: "Cost", value: `$${s.cost_usd.toFixed(2)}`, source: "harness-reported, last capture" });
  }
  return {
    available: rows.length > 0,
    reason: lane && !lane.available ? (lane.reason ?? "unavailable") : card ? null : "loading",
    rows,
  };
}

function clamp(text: string, max: number): string {
  const t = text.trim();
  return t.length <= max ? t : `${t.slice(0, max).trimEnd()}...`;
}

function deriveSiblings(i: NowInput): SiblingView[] {
  const terms = i.terms ?? [];
  const self = terms.find((t) => t.id === i.termId);
  if (!self) return [];
  const rows = joinHub(terms, i.live ?? [], { labels: {}, pins: new Set<string>() });
  const mine = rows.find((r) => r.term.id === i.termId)?.project;
  const marks = i.marks ?? {};
  return rows
    .filter((r) => r.term.id !== i.termId && !r.term.pane_dead && r.project === mine)
    .slice(0, SIBLING_CAP)
    .map((r) => {
      const m = marks[r.term.id];
      return {
        id: r.term.id,
        harness: r.term.harness,
        title: r.title,
        project: r.project,
        state: r.live?.state ?? r.term.state ?? "unknown",
        blocked: r.tier === "needs",
        mark: m?.asks ? "asks" : m?.finishedUnseen ? "finished" : null,
      };
    });
}

/** Build the Now tab's sections, in `NOW_SECTION_ORDER`, each with its freshness. */
export function buildNow(i: NowInput): NowModel {
  const linked = !!i.sid;
  const out: NowSection[] = [];
  const row = i.row ?? null;
  if (linked) {
    if (row?.blocked) {
      out.push({ id: "needs", title: "Needs you", fresh: LIVE_FRESH, needs: deriveNeeds(row, i.conv, i.nowUnix) });
    }
    const act = deriveActivity(row, i.card, i.nowUnix);
    const parts: string[] = [];
    if (act.lastTool || act.background) parts.push(LIVE_FRESH);
    if (act.subagents !== null) parts.push(`subagents ${CAPTURE_FRESH}`);
    out.push({ id: "activity", title: "Activity", fresh: parts.length ? parts.join("; ") : LIVE_FRESH, activity: act });
    out.push({ id: "status", title: "Status", fresh: LIVE_FRESH, chips: statusChips(row?.status) });
    const o = i.card?.outcome;
    if (o?.available && typeof o.last_assistant_text === "string" && o.last_assistant_text.trim() !== "") {
      out.push({ id: "answer", title: "Latest answer", fresh: CAPTURE_FRESH, text: clamp(o.last_assistant_text, ANSWER_CLAMP) });
    }
    if (i.asks) {
      out.push({ id: "asks", title: "Asks for you", fresh: SLATE_FRESH, open: [...i.asks] });
    }
  }
  if (i.termsOk !== false) {
    out.push({ id: "siblings", title: "Also on this project", fresh: TERMS_FRESH, siblings: deriveSiblings(i) });
  }
  if (linked) {
    out.push({ id: "stats", title: "Session stats", fresh: CAPTURE_FRESH, stats: deriveStats(i.card, row) });
  }
  out.push({ id: "queue", title: "Prompt queue", fresh: "" });
  out.sort((a, b) => NOW_SECTION_ORDER.indexOf(a.id) - NOW_SECTION_ORDER.indexOf(b.id));
  return { linked, sections: out };
}

/** The Now tab's badge: 1 iff the bound session is blocked on the operator. */
export function nowBadge(row: LiveStatusRow | null | undefined): number | undefined {
  return row?.blocked ? 1 : undefined;
}
