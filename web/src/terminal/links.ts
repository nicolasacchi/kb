// FUTERMFULL — http/https links in the terminal buffer. Pure over narrow row
// sources so the regex, the trailing-punctuation trim and the wrapped-row join
// are unit-testable without xterm. A link spanning wrapped rows is joined; the
// wide-character case (a double-width cell before the URL on its row) is not
// compensated: the range would start one column early.

export interface LinkMatch {
  /** Offsets into the joined text, end exclusive. */
  start: number;
  end: number;
  url: string;
}

const URL_RE = /https?:\/\/[^\s<>"'`\u0000-\u001f]+/g;
const TRAILING = /[.,;:!?'"’”]$/;
const PAIRS: Record<string, string> = { ")": "(", "]": "[", "}": "{" };

function count(s: string, ch: string): number {
  let n = 0;
  for (const c of s) if (c === ch) n += 1;
  return n;
}

/** Trim sentence punctuation and an unbalanced closing bracket from a raw match. */
export function trimUrl(raw: string): string {
  let u = raw;
  for (;;) {
    if (TRAILING.test(u)) {
      u = u.slice(0, -1);
      continue;
    }
    const last = u[u.length - 1];
    const open = last === undefined ? undefined : PAIRS[last];
    if (open && count(u, last) > count(u, open)) {
      u = u.slice(0, -1);
      continue;
    }
    return u;
  }
}

export function findUrls(text: string): LinkMatch[] {
  const out: LinkMatch[] = [];
  for (const m of text.matchAll(URL_RE)) {
    const url = trimUrl(m[0]);
    // A bare scheme is not a link.
    if (!/^https?:\/\/[^/?#]+/.test(url)) continue;
    out.push({ start: m.index!, end: m.index! + url.length, url });
  }
  return out;
}

export interface BufRow {
  /** Full-width text (not right-trimmed) for a row that continues on the next one. */
  text: string;
  wrapped: boolean;
}

export interface RowSource {
  length: number;
  /** 0-based buffer line; `wrapped` = continues the previous row. */
  row(y: number): BufRow | null;
}

export interface LinkRange {
  url: string;
  /** 1-based inclusive cell positions, as xterm's ILink range. */
  start: { x: number; y: number };
  end: { x: number; y: number };
}

/**
 * Links that touch buffer line `y0` (0-based): the logical line (the row and
 * every wrapped continuation around it) is joined, scanned, and each match
 * mapped back to rows.
 */
export function linksAtRow(src: RowSource, y0: number, cols: number): LinkRange[] {
  if (y0 < 0 || y0 >= src.length) return [];
  let first = y0;
  while (first > 0 && src.row(first)?.wrapped) first -= 1;
  const rows: string[] = [];
  let y = first;
  for (;;) {
    const r = src.row(y);
    if (!r) break;
    rows.push(r.text);
    const next = y + 1 < src.length ? src.row(y + 1) : null;
    if (!next?.wrapped) break;
    y += 1;
  }
  // Every row but the last is a full row of `cols` cells.
  const joined = rows.map((t, i) => (i < rows.length - 1 ? t.padEnd(cols, " ").slice(0, cols) : t)).join("");
  const at = (off: number): { x: number; y: number } => ({ x: (off % cols) + 1, y: first + Math.floor(off / cols) + 1 });
  const out: LinkRange[] = [];
  for (const m of findUrls(joined)) {
    const s = at(m.start);
    const e = at(m.end - 1);
    const line = y0 + 1;
    if (line < s.y || line > e.y) continue;
    out.push({ url: m.url, start: s, end: e });
  }
  return out;
}

/** Open a clicked link: http(s) only, a new tab, no opener, no referrer. */
export function openExternal(url: string, win: { open: (u: string, target: string, features: string) => unknown }): void {
  if (!/^https?:\/\//i.test(url)) return;
  win.open(url, "_blank", "noopener,noreferrer");
}
