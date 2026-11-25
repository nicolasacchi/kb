// RAILKNOW — pure helpers for the Knowledge tab. No fetching, no React.

import type { SessionRecallHit } from "../../../api/generated/SessionRecallHit";
import type { ResearchOut } from "../../../api/generated/ResearchOut";
import { artifactHref } from "../../../lib/artifactHref";

/** The most characters one pasted reference may carry. */
export const REF_MAX = 400;
/** Research rows shown before "Showing N of M". */
export const RESEARCH_CAP = 50;

/** A memory row as the lanes show it (injected, written, or a search hit). */
export interface MemoryRowData {
  kb: string;
  id: string;
  title?: string;
  source_relative?: string;
  summary?: string;
  pos?: number;
  used?: boolean;
  /** Only the injected lane carries a rank slot. */
  showRank?: boolean;
}

export const UNRESOLVED_TITLE = "(memory no longer resolves)";

export function rowTitle(r: Pick<MemoryRowData, "title">): string {
  return r.title && r.title.trim() !== "" ? r.title : UNRESOLVED_TITLE;
}

/**
 * The rank label. A number only when the capture recorded one; otherwise the
 * literal "rank unknown". Never derived from list position.
 */
export function rankLabel(pos: number | undefined): string {
  return typeof pos === "number" && Number.isFinite(pos) ? `#${pos}` : "rank unknown";
}

/**
 * The single-line reference pasted at the prompt: `[<title>] (<kb>/<rel>)`,
 * every whitespace run (newlines, CR, tabs) collapsed to one space, trimmed,
 * capped. Never ends with a newline, so a paste can never submit.
 */
export function flattenRef(r: Pick<MemoryRowData, "title" | "kb" | "source_relative">): string {
  const where = r.source_relative ? `${r.kb}/${r.source_relative}` : r.kb;
  const raw = `[${rowTitle(r)}] (${where})`;
  const flat = raw.replace(/\s+/g, " ").trim();
  return flat.length > REF_MAX ? flat.slice(0, REF_MAX).trimEnd() : flat;
}

export function openHref(r: Pick<MemoryRowData, "kb" | "source_relative">): string | null {
  return r.source_relative ? artifactHref(r.kb, r.source_relative) : null;
}

export function fromRecall(h: SessionRecallHit): MemoryRowData {
  return {
    kb: h.kb,
    id: h.id,
    title: h.title,
    source_relative: h.source_relative,
    pos: h.pos,
    used: h.used,
    showRank: true,
  };
}

export const RESEARCH_LABELS: Record<string, string> = {
  kb_search: "kb",
  web: "web",
  skill: "skill",
  subagent: "subagent",
  plan_span: "plan",
  artifact_open: "open",
};

export interface ResearchGroup {
  kind: string;
  label: string;
  rows: ResearchOut[];
}

/** Group the first RESEARCH_CAP rows by kind, first-seen order. */
export function groupResearch(rows: ResearchOut[]): { groups: ResearchGroup[]; shown: number; total: number } {
  const shownRows = rows.slice(0, RESEARCH_CAP);
  const groups: ResearchGroup[] = [];
  for (const r of shownRows) {
    let g = groups.find((x) => x.kind === r.kind);
    if (!g) {
      g = { kind: r.kind, label: RESEARCH_LABELS[r.kind] ?? r.kind, rows: [] };
      groups.push(g);
    }
    g.rows.push(r);
  }
  return { groups, shown: shownRows.length, total: rows.length };
}

export function sessionHref(sid: string): string {
  return `/sessions/${encodeURIComponent(sid)}`;
}
