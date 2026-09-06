import type { Anchor } from "../api/client";

// Relative "Ns/Nm/Nh/Nd ago" label for a kb-comments/1 ISO timestamp.
// (History events use lib/time.ts — different shape: unix seconds + a
// calendar-style label.)
export function relTime(iso: string): string {
  const then = new Date(iso).getTime();
  const sec = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (sec < 60) return `${sec}s ago`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ago`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h ago`;
  return `${Math.floor(sec / 86400)}d ago`;
}

// Short human label for a comment's (or pending compose's) anchor, minus its
// own leading glyph — the bare "where" (file/chapter path/section id/quoted
// selection). W1.reader's citation template (lib/quote.ts) supplies its own
// "§" separator ("<title> § <label>"), so it composes this instead of
// `anchorLabel` — using `anchorLabel`'s section case there would double up
// ("Title § § sec-id").
export function anchorLocationLabel(c: { anchor: Anchor }): string {
  switch (c.anchor.kind) {
    case "file":
      return "file";
    case "chapter":
      return c.anchor.path.length > 28
        ? "…" + c.anchor.path.slice(-26)
        : c.anchor.path;
    case "section":
      return c.anchor.id.slice(0, 24);
    case "selection":
      return `“${c.anchor.snippet.slice(0, 22)}…”`;
  }
}

// Short human label for a comment's (or pending compose's) anchor.
export function anchorLabel(c: { anchor: Anchor }): string {
  return c.anchor.kind === "section"
    ? `§ ${anchorLocationLabel(c)}`
    : anchorLocationLabel(c);
}
