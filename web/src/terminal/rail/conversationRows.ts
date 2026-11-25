// CONVRAIL — pure helpers for the Conversation rail: filter, excerpt,
// freshness text, copy text and the reader href. No React, no fetching.

import type { ConvRow } from "../../api/generated/ConvRow";
import { artifactHref } from "../../lib/artifactHref";

export type ConvFilter = "all" | "questions" | "answers";

export const CONV_FILTERS: readonly ConvFilter[] = ["all", "questions", "answers"];

/** questions = rows with at least one ask; answers = rows with a final answer. */
export function filterRows(rows: readonly ConvRow[], filter: ConvFilter): ConvRow[] {
  if (filter === "questions") return rows.filter((r) => r.asks.length > 0);
  if (filter === "answers") return rows.filter((r) => r.answer !== null);
  return [...rows];
}

export const ANSWER_EXCERPT_CHARS = 280;

/** A bounded excerpt: whitespace runs collapsed, cut at a word edge with an ellipsis. */
export function excerpt(text: string, max: number = ANSWER_EXCERPT_CHARS): string {
  const flat = text.replace(/\s+/g, " ").trim();
  if (flat.length <= max) return flat;
  const cut = flat.slice(0, max);
  const sp = cut.lastIndexOf(" ");
  return `${(sp > max * 0.6 ? cut.slice(0, sp) : cut).trimEnd()}…`;
}

/**
 * The session's last-event time (unix SECONDS, `sessions.ended_at`; the
 * capture file mtime only when no event time exists), parsed from the
 * server's `capture_gen` (`"{time}-{turns_total}"`). null when unparsable or 0.
 */
export function captureMtime(captureGen: string): number | null {
  const m = /^(\d+)-\d+$/.exec(captureGen);
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isSafeInteger(n) && n > 0 ? n : null;
}

/** "As of last capture, N min ago" with hour/day scaling; never claims liveness. */
export function freshnessText(captureGen: string, nowMs: number): string {
  const t = captureMtime(captureGen);
  if (t === null) return "Capture time unknown";
  const secs = Math.max(0, Math.floor(nowMs / 1000) - t);
  let ago: string;
  if (secs < 60) ago = "just now";
  else if (secs < 3600) ago = `${Math.floor(secs / 60)} min ago`;
  else if (secs < 86_400) ago = `${Math.floor(secs / 3600)} h ago`;
  else ago = `${Math.floor(secs / 86_400)} d ago`;
  return `As of last capture, ${ago}`;
}

export const LIVE_CAPTURE_HINT = "Live capture may be off: new turns appear after the next capture.";

/** "Showing last K of T turns" when the window is narrower than the capture; else null. */
export function windowCaption(total: number, shown: number): string | null {
  return total > shown ? `Showing last ${shown} of ${total} turns` : null;
}

/** Plain text of one turn for the clipboard: prompt, blank line, answer. */
export function copyText(row: ConvRow): string {
  return row.answer === null ? row.prompt : `${row.prompt}\n\n${row.answer}`;
}

/** The reader URL at one turn (`?turn=t-<uuid12>` is used verbatim by the reader). */
export function jumpHref(kb: string, sourceRelative: string, turnId: string): string {
  return artifactHref(kb, sourceRelative, { turn: turnId });
}
