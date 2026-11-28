// CONVNEED — what the terminal's bound session is waiting on, derived (never
// from terminal bytes) from the live-status row and the capture-scoped
// Conversation projection. Pure: no fetching, no DOM. Every string here is
// already scrubbed by the two reads it comes from; it is rendered as text.

import type { LiveStatusRow } from "../../api/sessions";
import type { ConversationResult } from "../../api/sessions";

export type NeedsKind = "question" | "plan" | "permission" | "attention";
export type NeedsSource = "capture" | "beat" | "none";

export interface NeedsYou {
  kind: NeedsKind;
  text: string;
  options: string[];
  tool?: string;
  source: NeedsSource;
  sinceUnix?: number;
  lastLine?: string;
}

export const GENERIC_NEEDS_TEXT = "Waiting for you";

/** The capture's mtime from `capture_gen` (`{mtime_unix}-{turns_total}`); null when unparsable. */
export function captureMtime(gen: string): number | null {
  const m = /^(\d+)-\d+$/.exec(gen);
  if (!m) return null;
  const n = Number(m[1]);
  return Number.isSafeInteger(n) ? n : null;
}

function nonEmpty(s: string | undefined): string | undefined {
  const t = s?.trim();
  return t ? t : undefined;
}

export function deriveNeeds(
  row: LiveStatusRow | undefined,
  conv: ConversationResult | undefined,
): NeedsYou | null {
  if (!row || row.blocked !== true) return null;
  const tool = nonEmpty(row.blocked_tool);
  const reason = nonEmpty(row.detail_reason);
  const lastLine = nonEmpty(row.last_line);
  const since = row.blocked_since_unix;
  const base = {
    ...(tool ? { tool } : {}),
    ...(since !== undefined ? { sinceUnix: since } : {}),
  };

  // (a) the pending ask, only from a capture at least as new as the block: an
  // older capture's pending ask belongs to an earlier question.
  if (conv?.kind === "ok" && since !== undefined) {
    const mtime = captureMtime(conv.conversation.capture_gen);
    if (mtime !== null && mtime >= since) {
      const rows = conv.conversation.rows;
      const last = rows.length > 0 ? rows[rows.length - 1] : undefined;
      const ask = last?.asks.find((a) => a.pending);
      if (ask && nonEmpty(ask.q)) {
        return {
          kind: ask.kind === "plan" ? "plan" : "question",
          text: ask.q,
          options: ask.options,
          source: "capture",
          ...base,
        };
      }
    }
  }

  const kind: NeedsKind = tool || reason ? "permission" : "attention";
  if (reason) {
    return { kind, text: reason, options: [], source: "beat", ...base, ...(lastLine ? { lastLine } : {}) };
  }
  return {
    kind,
    text: GENERIC_NEEDS_TEXT,
    options: [],
    source: "none",
    ...base,
    ...(lastLine ? { lastLine } : {}),
  };
}
