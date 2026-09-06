// Client-side OPTIMISTIC mirror of the server's task toggle
// (`kb_core::notes::scan_tasks`, which parses with comrak). Flips the
// `index`-th GFM task item (document order, fenced code skipped) to `on` and
// returns the new body — used only for the instant pre-flip before the server
// response lands; `useNote.toggleTask` then replaces this with the
// authoritative `NoteMutate.body_md`. Because it's optimistic-only it doesn't
// need a full CommonMark parser — it just has to agree with the server on the
// common GFM forms (bullet / ordered / blockquoted, any fence length). Pinned
// against the same fixtures the Rust side uses in `flipTask.test.ts`.
//
// History: this lived inline in `useNotes.ts` with two bugs — it stored a
// 3-char fence regardless of the opener's length, and accepted `- [ ]x` (no
// whitespace after `]`, which GFM rejects). Both are fixed here.

// A GFM task line: optional blockquote prefix + indentation, a bullet
// (`-`/`*`/`+`) or ordered (`1.`/`1)`) marker, 1+ spaces, then `[ ]`/`[x]`/`[X]`
// followed by whitespace or end-of-line. Group 1 = everything up to and
// including `[` (preserves original spacing on rewrite); group 2 = the symbol
// char; group 3 = `]` + the rest of the line.
const TASK_RE = /^(\s*(?:>\s?)*\s*(?:[-*+]|\d+[.)]) +\[)([ xX])(\](?:\s.*)?)$/;
const FENCE_RE = /^(`{3,}|~{3,})/;

export function flipTask(body: string, index: number, on: boolean): string {
  const lines = body.split("\n");
  let count = -1;
  let fence: { ch: string; len: number } | null = null;
  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].replace(/^\s+/, "");
    if (fence) {
      // Close: a run of >= the opener's length of the SAME fence char, with
      // only trailing whitespace (an info string can't close).
      const close = trimmed.match(/^(`{3,}|~{3,})\s*$/);
      if (close && close[1][0] === fence.ch && close[1].length >= fence.len) {
        fence = null;
      }
      continue;
    }
    const open = trimmed.match(FENCE_RE);
    if (open) {
      fence = { ch: open[1][0], len: open[1].length };
      continue;
    }
    const m = lines[i].match(TASK_RE);
    if (m) {
      count++;
      if (count === index) {
        lines[i] = m[1] + (on ? "x" : " ") + m[3];
        break;
      }
    }
  }
  return lines.join("\n");
}
