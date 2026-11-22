// FUKEYBAR — operator quick replies: a chip row of canned messages.
//
// Client-only (browser localStorage, no server state). A tap hands the text to
// the composer's own send path (`planSend`), so every guard applies: a reply
// starting with `/` or `@` is pasted only, a blocked session pastes and holds,
// a multi-line reply is refused without bracketed paste. Nothing here talks
// to the terminal itself.

import { useState } from "react";
import { createPortal } from "react-dom";

export const REPLIES_KEY = "kb.term.quickReplies";
export const REPLIES_MAX = 6;
export const REPLY_MAX_CHARS = 200;
export const DEFAULT_REPLIES: readonly string[] = ["yes, proceed", "continue", "run the tests", "commit and push"];

/** Trim, drop blanks and over-long lines, cap the count. Pure. */
export function normalizeReplies(lines: readonly unknown[]): string[] {
  const out: string[] = [];
  for (const l of lines) {
    if (typeof l !== "string") continue;
    const t = l.trim();
    if (t === "" || t.length > REPLY_MAX_CHARS || /[\r\n]/.test(t)) continue;
    out.push(t);
    if (out.length >= REPLIES_MAX) break;
  }
  return out;
}

export function loadReplies(): string[] {
  try {
    const raw = localStorage.getItem(REPLIES_KEY);
    if (raw === null) return [...DEFAULT_REPLIES];
    const v: unknown = JSON.parse(raw);
    return Array.isArray(v) ? normalizeReplies(v) : [...DEFAULT_REPLIES];
  } catch {
    return [...DEFAULT_REPLIES];
  }
}

function saveReplies(list: string[]): void {
  try {
    localStorage.setItem(REPLIES_KEY, JSON.stringify(list));
  } catch {
    // storage full or blocked: the in-memory list still works this visit
  }
}

export interface QuickRepliesProps {
  /** Send this text exactly as the composer's Send would. */
  onSend: (text: string) => void;
  disabled?: boolean;
  /**
   * POLISH2 — a landscape phone: the chips render inside this element (the key
   * bar's scroller, after the keys) so keys and replies share one scrolling row.
   * Null/absent: the chips render in place, above the composer.
   */
  host?: HTMLElement | null;
}

// Buttons act on click but never take focus on press (the soft keyboard stays up).
const keepFocus = {
  onPointerDown: (e: { preventDefault(): void }) => e.preventDefault(),
  onMouseDown: (e: { preventDefault(): void }) => e.preventDefault(),
};

export function QuickReplies({ onSend, disabled = false, host = null }: QuickRepliesProps) {
  const [replies, setReplies] = useState<string[]>(loadReplies);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");

  if (editing) {
    return (
      <div className="kb-drive__replies kb-drive__replies--edit" data-testid="kb-drive-replies-editor">
        <textarea
          className="kb-drive__input"
          data-testid="kb-drive-replies-text"
          aria-label="Quick replies, one per line"
          rows={4}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
        />
        <button
          type="button"
          className="kb-drive__key"
          data-testid="kb-drive-replies-save"
          onClick={() => {
            const next = normalizeReplies(draft.split("\n"));
            setReplies(next);
            saveReplies(next);
            setEditing(false);
          }}
        >
          Save
        </button>
        <button
          type="button"
          className="kb-drive__key"
          data-testid="kb-drive-replies-cancel"
          onClick={() => setEditing(false)}
        >
          Cancel
        </button>
      </div>
    );
  }

  const chips = (
    <div className="kb-drive__replies" data-testid="kb-drive-replies" role="toolbar" aria-label="Quick replies">
      {replies.map((r, i) => (
        <button
          key={`${i}-${r}`}
          type="button"
          className="kb-drive__key"
          data-testid={`kb-drive-reply-${i}`}
          disabled={disabled}
          {...keepFocus}
          onClick={() => onSend(r)}
        >
          {r}
        </button>
      ))}
      <button
        type="button"
        className="kb-drive__key"
        data-testid="kb-drive-replies-edit"
        aria-label="Edit quick replies"
        {...keepFocus}
        onClick={() => {
          setDraft(replies.join("\n"));
          setEditing(true);
        }}
      >
        Edit
      </button>
    </div>
  );
  return host ? createPortal(chips, host) : chips;
}
