import type { ComponentType, SVGProps } from "react";
import type { Comment } from "../api/client";
import { Icon } from "./icons";

// R3 — one-tap responses to Claude's comments. Two flavours, both posting
// a "you" reply on click:
//   - CANNED: a fixed set shown once per open thread whose last word was
//     Claude's (the ball is in your court).
//   - choices: Claude-authored buttons attached to a specific comment or
//     reply (rendered at their attachment point).
// SH.I2 — the two CANNED entries with a drawn equivalent (agree/disagree)
// get a leading icon; server-authored `choices` labels are arbitrary text
// from Claude and carry no `glyph`, so they render unchanged.
export const CANNED: {
  label: string;
  reply: string;
  glyph?: ComponentType<SVGProps<SVGSVGElement>>;
}[] = [
  { label: "Agree", reply: "Agree.", glyph: Icon.ThumbUp },
  { label: "Disagree", reply: "Disagree.", glyph: Icon.ThumbDown },
  { label: "Tell me more", reply: "Tell me more." },
];

// True when the last thing said on the thread was Claude's — i.e. the
// canned quick-reply bar is worth showing.
export function threadAwaitsResponse(c: Comment): boolean {
  const last = c.replies.length
    ? c.replies[c.replies.length - 1].author
    : c.author;
  return last === "claude";
}

export function QuickButtons({
  items,
  onChoose,
}: {
  items: {
    label: string;
    reply: string;
    resolve?: boolean;
    glyph?: ComponentType<SVGProps<SVGSVGElement>>;
  }[];
  onChoose: (body: string, resolve: boolean) => void;
}) {
  if (items.length === 0) return null;
  return (
    <div className="cp__quick">
      {items.map((it, i) => {
        const Glyph = it.glyph;
        return (
          <button
            key={i}
            type="button"
            className={`cp__quick-btn ${it.resolve ? "cp__quick-btn--resolve" : ""}`}
            title={it.resolve ? "reply + resolve" : "quick reply"}
            onClick={() => onChoose(it.reply, !!it.resolve)}
          >
            {Glyph && <Glyph aria-hidden="true" />} {it.label}
          </button>
        );
      })}
    </div>
  );
}
