// MI-W4.2d — THE AGENT'S-EYE SIMULATOR's pure rendering core.
//
// This renders the EXACT text `plugins/kb-memory/hooks/kb-recall.sh`
// injects into a turn as `hookSpecificOutput.additionalContext` — the same
// jq filter, ported line-for-line, so the simulator shows literally what
// the agent would see, not an approximation:
//
//   Relevant memories from kb (recall — these persist across sessions):
//   - <title>  [<kb>]  (id <id>, read <pct>% — stopped at <section>)
//       ↳ <summary, truncated to 220 chars>
//   - <title>  [<kb>]  (id <id>, unread)
//
// The RANKING itself is never reimplemented here — the caller drives this
// off the real `GET /api/memory/recall?...&limit=5` response (same
// scope/limit the hook's own `kb recall "$prompt" --scope all --limit 5`
// call uses), and this module only formats the hits it's given.

import type { RecallHit } from "../api/client";

export const INJECTION_LIMIT = 5;

const HEADER = "Relevant memories from kb (recall — these persist across sessions):";
const SUMMARY_MAX_CHARS = 220;

/** One hit's rendered line(s), matching kb-recall.sh's jq map() exactly. */
export function formatInjectionLine(h: RecallHit): string {
  let line = `- ${h.title}  [${h.kb}]  (id ${h.id}`;
  if (h.read_pct != null) {
    line += `, read ${h.read_pct}%`;
    if (h.stopped_at != null) line += ` — stopped at ${h.stopped_at}`;
  } else {
    line += `, unread`;
  }
  line += `)`;
  if (h.summary) {
    line += `\n    ↳ ${h.summary.slice(0, SUMMARY_MAX_CHARS)}`;
  }
  return line;
}

/**
 * The full injected block, or `null` when there's nothing to inject — the
 * hook's own `[ -n "$block" ] || exit 0` early-out: an empty hit list
 * injects NOTHING, not an empty header.
 */
export function formatInjectionBlock(hits: RecallHit[]): string | null {
  if (hits.length === 0) return null;
  const lines = hits.map(formatInjectionLine);
  return `${HEADER}\n${lines.join("\n")}`;
}
