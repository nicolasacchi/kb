// OL L3 — the Front Page band: pure composition for the unfiltered Library's
// opening band (components/FrontPage.tsx). Everything here is deterministic
// and side-effect free (storage and clocks are injected), in the same posture
// as lib/broadsheet.ts, whose `scoreDoc` ranking it reuses. No numbers are
// invented: every count is either server-authoritative or says so.

import type { DocSummary, EchoOut, ResurfaceItem } from "../api/client";
import { scoreDoc, type ScoreExplain } from "./broadsheet";
import { isIndexPage } from "./derive";
import { relativeAge } from "./time";

// ── 1. when the band shows ────────────────────────────────────────────────

export type FrontPageGate = {
  activeFolder: string | null | undefined;
  tagCount: number;
  capCount: number;
  q: string | null | undefined;
  category: string | null | undefined;
  readFilter: unknown;
  since: string;
  fromUnix: number | null | undefined;
  toUnix: number | null | undefined;
  sort: string;
  view: string;
};

/// The band renders iff the Library is unfiltered: the lobby's old gate
/// (no folder/tags/caps/q/category/read, since=all, no from/to, sort=recent)
/// AND the view is the grid or list. Any filter, query or sort is intent, and
/// the band's whole point is the moment before intent.
export function showsFrontPage(g: FrontPageGate): boolean {
  return (
    !g.activeFolder &&
    g.tagCount === 0 &&
    g.capCount === 0 &&
    !g.q &&
    !g.category &&
    !g.readFilter &&
    g.since === "all" &&
    g.fromUnix == null &&
    g.toUnix == null &&
    g.sort === "recent" &&
    (g.view === "grid" || g.view === "list")
  );
}

// ── 2. scope + the fleet kb walk ──────────────────────────────────────────

/// The daemon's fan-out cap (CLAUDE.md #28, `[server] fanout_cap` default 8):
/// the client never walks more kbs than a single federated request would.
export const FLEET_KB_CAP = 8;

/// The kbs a fleet-wide band reads, in the list's own (stable) order. Memory
/// corpora are not artifact shelves, so they never contribute stories.
export function fleetKbs(
  kbs: ReadonlyArray<{ name: string; memory_scope?: string | null }>,
): string[] {
  return kbs
    .filter((k) => !k.memory_scope)
    .slice(0, FLEET_KB_CAP)
    .map((k) => k.name);
}

/// Ids collide across corpora (#7 v2), so anything keyed per artifact in a
/// merged view keys on (kb, id).
export function rowKey(kb: string, id: string): string {
  return `${kb}\u0000${id}`;
}

export type FrontRow = { kb: string; doc: DocSummary };

// ── 3. visits ─────────────────────────────────────────────────────────────

export type VisitState = { last: number | null; prev: number | null };

/// Rotate the visit record once per browser session: the first mount of a
/// session moves `last` into `prev` and stamps `now`; later mounts in the
/// same session leave it alone, so "since your last visit" stays stable
/// while you move around (and across reloads) instead of resetting to zero
/// the moment you look at it. `since` is `null` when no earlier visit is on
/// record in this browser.
export function rotateVisit(
  state: VisitState,
  sessionSeen: boolean,
  nowUnix: number,
): { state: VisitState; since: number | null } {
  if (sessionSeen) return { state, since: state.prev };
  const next = { last: nowUnix, prev: state.last };
  return { state: next, since: next.prev };
}

export function parseVisit(raw: string | null): VisitState {
  if (!raw) return { last: null, prev: null };
  try {
    const v = JSON.parse(raw) as Partial<VisitState> | null;
    const num = (x: unknown) => (typeof x === "number" && Number.isFinite(x) ? x : null);
    return { last: num(v?.last), prev: num(v?.prev) };
  } catch {
    return { last: null, prev: null };
  }
}

/// Storage keys are per scope: one kb, or `*` for the fleet band.
export const visitKey = (scope: string | null) => `kb:frontpage:visit:${scope ?? "*"}`;
export const collapsedKey = (scope: string | null) =>
  `kb:frontpage:collapsed:${scope ?? "*"}`;

export function countNewSince(
  groups: ReadonlyArray<{ rows: ReadonlyArray<DocSummary>; hasMore: boolean }>,
  since: number | null,
): { count: number; exact: boolean } | null {
  if (since == null) return null;
  let count = 0;
  let exact = true;
  for (const g of groups) {
    let n = 0;
    for (const d of g.rows) if ((d.mtime_unix ?? 0) > since) n++;
    count += n;
    // Rows arrive newest-first. If every loaded row is new and more exist,
    // the true count is a lower bound, not the number.
    if (g.hasMore && g.rows.length > 0 && n === g.rows.length) exact = false;
  }
  return { count, exact };
}

// ── 4. masthead text ──────────────────────────────────────────────────────

const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS = [
  "January", "February", "March", "April", "May", "June", "July",
  "August", "September", "October", "November", "December",
];

/// "Friday 16 October 2026" in the reader's local calendar. Spelled out from
/// fixed tables so it never depends on the runtime locale.
export function mastheadDate(nowMs: number): string {
  const d = new Date(nowMs);
  return `${WEEKDAYS[d.getDay()]} ${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
}

export type EditionInput = {
  /// Server-authoritative artifact count (envelope total / summed doc_count).
  total: number;
  /// Corpora counted, when the band is fleet-wide.
  corpora?: number;
  /// More shelf corpora exist than the band walks (`FLEET_KB_CAP`).
  corporaCapped?: boolean;
  /// Words are summed from the loaded rows, so they are only the truth when
  /// every row is loaded (`hasMore === false`); otherwise the term is dropped.
  words: number | null;
  newSince: { count: number; exact: boolean } | null;
};

/// "12 artifacts · 4,200 words · 3 new since your last visit". Terms that
/// cannot be stated truthfully are omitted, never estimated.
export function editionLine(e: EditionInput): string {
  const parts: string[] = [];
  const corpora =
    e.corpora == null
      ? ""
      : e.corporaCapped
        ? ` across the first ${e.corpora} corpora`
        : ` across ${e.corpora} ${e.corpora === 1 ? "corpus" : "corpora"}`;
  parts.push(`${e.total.toLocaleString()} artifact${e.total === 1 ? "" : "s"}${corpora}`);
  if (e.words != null) {
    parts.push(`${e.words.toLocaleString()} word${e.words === 1 ? "" : "s"}`);
  }
  if (e.newSince == null) {
    parts.push("no earlier visit on record here");
  } else {
    parts.push(
      `${e.newSince.count.toLocaleString()}${e.newSince.exact ? "" : "+"} new since your last visit`,
    );
  }
  return parts.join(" · ");
}

// ── 5. the page ───────────────────────────────────────────────────────────

export type FrontLead = {
  kb: string;
  id: string;
  title: string;
  source_relative: string;
  /// Why it surfaces: the resurface queue's own first reason, or (fallback)
  /// a plain statement of what the ranking saw.
  dateline: string;
  from: "resurface" | "ranked";
  /// The matching loaded row, when there is one (gives the dek/words/folder).
  row: DocSummary | null;
  explain: ScoreExplain | null;
};

export type FrontPageModel = {
  lead: FrontLead | null;
  features: FrontRow[];
  /// "new": items changed since the last visit. "ranked": the broadsheet's
  /// own ranking (nothing, or too little, is new).
  featuresKind: "new" | "ranked";
};

export const FEATURES_MAX = 4;
export const FEATURES_MIN_NEW = 2;

type ComposeInput = {
  rows: ReadonlyArray<FrontRow>;
  /// Open-comment counts keyed by `rowKey`.
  openMass: ReadonlyMap<string, number>;
  resurface: ReadonlyArray<{ kb: string; item: ResurfaceItem }>;
  since: number | null;
  nowMs: number;
};

export function composeFrontPage(input: ComposeInput): FrontPageModel {
  const ranked = input.rows
    .map((r) => ({
      r,
      explain: scoreDoc(r.doc, input.openMass.get(rowKey(r.kb, r.doc.id)) ?? 0, input.nowMs),
    }))
    .sort(
      (a, b) =>
        b.explain.score - a.explain.score ||
        a.r.kb.localeCompare(b.r.kb) ||
        a.r.doc.id.localeCompare(b.r.doc.id),
    );

  // Lead: the top resurface item (the queue's own score, kb/id tiebreak);
  // fallback = the ranking's top story.
  const queue = [...input.resurface].sort(
    (a, b) =>
      b.item.score - a.item.score ||
      a.kb.localeCompare(b.kb) ||
      a.item.id.localeCompare(b.item.id),
  );
  let lead: FrontLead | null = null;
  if (queue.length > 0) {
    const { kb, item } = queue[0];
    const hit = ranked.find((x) => x.r.kb === kb && x.r.doc.id === item.id);
    lead = {
      kb,
      id: item.id,
      title: item.title,
      source_relative: item.source_relative,
      dateline: item.reasons[0] ?? "Waiting for you",
      from: "resurface",
      row: hit?.r.doc ?? null,
      explain: hit?.explain ?? null,
    };
  } else if (ranked.length > 0) {
    const { r, explain } = ranked[0];
    const age = relativeAge(r.doc.mtime_unix, input.nowMs);
    const bits = age === "" ? [] : [age === "now" ? "edited just now" : `edited ${age} ago`];
    if (explain.openComments > 0) {
      bits.push(`${explain.openComments} open comment${explain.openComments === 1 ? "" : "s"}`);
    }
    lead = {
      kb: r.kb,
      id: r.doc.id,
      title: r.doc.title || "(untitled)",
      source_relative: r.doc.source_relative,
      dateline: bits.length > 0 ? bits.join(" · ") : "Most weight lately",
      from: "ranked",
      row: r.doc,
      explain,
    };
  }

  const rest = ranked.filter(
    (x) => !(lead && x.r.kb === lead.kb && x.r.doc.id === lead.id),
  );
  const fresh =
    input.since == null
      ? []
      : rest.filter((x) => (x.r.doc.mtime_unix ?? 0) > (input.since as number));
  const useNew = fresh.length >= FEATURES_MIN_NEW;
  const pool = useNew ? fresh : rest;
  return {
    lead,
    features: pool.slice(0, FEATURES_MAX).map((x) => x.r),
    featuresKind: useNew ? "new" : "ranked",
  };
}

// ── 6. echoes ("on this day") ─────────────────────────────────────────────

/// "1y"/"2y" for whole years, "6mo" for the half-anniversary.
export function echoAge(monthsAgo: number): string {
  return monthsAgo % 12 === 0 ? `${monthsAgo / 12}y` : `${monthsAgo}mo`;
}

/// Artifact echoes deep-link to the reader (caller passes the shared
/// permalink builder); session echoes into the worklog, focused on that
/// capture. `null` = nothing to link to.
export function echoTarget(
  kb: string,
  item: EchoOut,
  artifactHref: (kb: string, rel: string) => string,
): string | null {
  if (item.kind === "worked") {
    return item.session_id
      ? `/sessions?kb=${encodeURIComponent(kb)}&focus=${encodeURIComponent(item.session_id)}`
      : null;
  }
  return item.source_relative ? artifactHref(kb, item.source_relative) : null;
}

// ── 7. the masthead's Index link ──────────────────────────────────────────

function newer(a: DocSummary, b: DocSummary): boolean {
  return (a.indexed_at_unix ?? a.mtime_unix ?? 0) > (b.indexed_at_unix ?? b.mtime_unix ?? 0);
}

/// The kb's landing page: a root-level `index.html` wins, else the most
/// recently indexed index page. (Moved from the retired PinnedIndexCard.)
export function pickIndexHero(docs: ReadonlyArray<DocSummary>): DocSummary | null {
  let root: DocSummary | null = null;
  let fallback: DocSummary | null = null;
  for (const d of docs) {
    if (!isIndexPage(d)) continue;
    const segs = d.path.split("/").filter(Boolean);
    if (segs.length === 1 && segs[0].toLowerCase() === "index.html") {
      if (root == null || newer(d, root)) root = d;
      continue;
    }
    if (fallback == null || newer(d, fallback)) fallback = d;
  }
  return root ?? fallback;
}
