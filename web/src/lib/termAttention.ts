// FUTERMATTN - the terminal attention signal's pure pieces: the "(n) " title prefix,
// the sanitized program title (OSC 0/2) and the bell rule. No DOM, no React.

/** Longest program title the panel header shows (characters). */
export const PROG_TITLE_MAX = 60;

/** `(n) <base>` when n sessions need the operator, else `<base>` unchanged. */
export function titleWithBadge(base: string, count: number): string {
  const n = Number.isFinite(count) ? Math.max(0, Math.floor(count)) : 0;
  return n > 0 ? `(${n}) ${base}` : base;
}

// C0/C1 controls and DEL, line/paragraph separators, bidi controls and the
// zero-width joiners a program could use to spoof the header.
// eslint-disable-next-line no-control-regex
const UNSAFE = /[\u0000-\u001f\u007f-\u009f\u2028\u2029\u200b-\u200f\u202a-\u202e\u2066-\u2069\ufeff]/g;

/** The title a program set with OSC 0/2, safe to render as TEXT: controls
 *  and bidi marks dropped, whitespace collapsed, capped. "" when nothing is left. */
export function sanitizeOscTitle(raw: unknown, max: number = PROG_TITLE_MAX): string {
  if (typeof raw !== "string") return "";
  const t = raw.replace(UNSAFE, " ").replace(/\s+/g, " ").trim();
  if (t.length <= max) return t;
  return `${t.slice(0, Math.max(0, max - 1)).trimEnd()}\u2026`;
}

/** A BEL needs the operator's eye only when they are not already looking at
 *  the terminal: the tab is hidden or the terminal does not have focus. */
export function bellNeedsAttention(s: { hidden: boolean; termFocused: boolean }): boolean {
  return s.hidden || !s.termFocused;
}
