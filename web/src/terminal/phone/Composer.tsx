// T18 — the native composer: a real <textarea> for the phone.
//
// xterm's hidden textarea has an Android IME bug (xterm.js issue 3600) and no
// autocorrect or dictation, so a phone types into THIS textarea instead and
// the text is pasted into the session (`planSend.ts` decides which calls).
// It is the ONLY focus-taking element of the Drive UI. Enter is never
// implied: it follows only a plain paste, and the runner still waits for a
// quiet pane and the server still refuses it while the session is blocked.
// Focusing, typing and the soft keyboard opening or closing never resize the
// pty (nothing here calls take-size or resize).

import { useEffect, useRef, useState } from "react";
import { Icon } from "@kb/ui/icons";
import { toast } from "../../lib/toast";
import {
  DriveError,
  UPLOAD_TYPES,
  sendText,
  uploadImage,
  uploadRefusal,
  type Drive,
} from "./driveApi";
import { CAPTION, planSend } from "./planSend";
import { QuickReplies } from "./QuickReplies";

/** A draft is kept per terminal for a day (client UI state, never terminal bytes). */
export const DRAFT_TTL_MS = 24 * 3600 * 1000;
const draftKey = (termId: string): string => `kb.term.draft.${termId}`;

export function loadDraft(termId: string): string {
  try {
    const raw = localStorage.getItem(draftKey(termId));
    if (raw === null) return "";
    const v = JSON.parse(raw) as { t?: unknown; at?: unknown };
    if (typeof v.t !== "string" || typeof v.at !== "number" || Date.now() - v.at > DRAFT_TTL_MS) {
      localStorage.removeItem(draftKey(termId));
      return "";
    }
    return v.t;
  } catch {
    return "";
  }
}

export function saveDraft(termId: string, text: string): void {
  try {
    if (text === "") localStorage.removeItem(draftKey(termId));
    else localStorage.setItem(draftKey(termId), JSON.stringify({ t: text, at: Date.now() }));
  } catch {
    // storage full or blocked: the draft just does not survive
  }
}

export interface ComposerProps {
  drive: Drive;
  /** The pane's `bracket_paste_flag`; null while the terminal row is unknown. */
  bracketPaste: boolean | null;
  /** The bound session is blocked on the operator (beat-derived). */
  blocked: boolean;
  /** A session is bound to this terminal (else Enter cannot be judged and stays off). */
  bound: boolean;
  caption: string | null;
  onCaption: (text: string | null) => void;
  onTyped?: () => void;
}

const EXT: Record<string, string> = {
  "image/png": "png",
  "image/jpeg": "jpg",
  "image/gif": "gif",
  "image/webp": "webp",
};

/** The chip text for a stored picture: `image-<first 8 of sha>.<ext>`. */
export function imageChip(sha256: string, mime: string): string {
  return `image-${sha256.slice(0, 8)}.${EXT[mime] ?? "img"}`;
}

export function Composer(props: ComposerProps) {
  const { drive, bracketPaste, blocked, bound, caption, onCaption, onTyped } = props;
  const [text, setText] = useState(() => loadDraft(drive.termId));
  const [busy, setBusy] = useState(false);
  const [chip, setChip] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    saveDraft(drive.termId, text);
  }, [drive.termId, text]);

  // `override` is a quick reply: the same plan and calls, but the draft is left alone.
  const send = async (override?: string): Promise<void> => {
    const body = override ?? text;
    const plan = planSend(body, { harness: drive.harness, bracketPaste, blocked });
    if (plan.kind === "empty") return;
    if (plan.kind === "refuse") {
      onCaption(CAPTION.refuse);
      return;
    }
    setBusy(true);
    onCaption(null);
    try {
      // Enter only for a plain paste, and only when a session is bound (an
      // unbound terminal's blocked state is unknown, so Enter stays off).
      const enter = plan.kind === "paste-enter" && bound;
      const r = await sendText(drive, plan.text, { enter });
      if (r.kind === "declined") return;
      if (override === undefined) setText("");
      onTyped?.();
      if (r.kind === "pasted-not-sent") {
        // The text IS in the pane: clear it, say so, and offer no resend.
        onCaption(CAPTION.pastedNotSent(r.detail));
        if (!r.conflict) toast.err(r.detail);
      } else if (plan.kind === "paste-only") {
        onCaption(CAPTION.pastedOnly);
      } else if (plan.kind === "paste-then-hold") {
        onCaption(CAPTION.holding);
      } else if (!bound) {
        onCaption(CAPTION.unbound);
      }
    } catch (e) {
      // Nothing was written: the text stays. A refused multi-line paste is
      // the one failure with its own words; the rest is a toast (invariant #32).
      if (e instanceof DriveError && e.type === "urn:kb:errors:terminal-refused") {
        onCaption(CAPTION.refuse);
      } else {
        toast.err(e instanceof DriveError ? e.detail : "Could not reach the terminal");
      }
    } finally {
      setBusy(false);
    }
  };

  const attach = async (file: File): Promise<void> => {
    const refusal = uploadRefusal(file);
    if (refusal !== null) {
      toast.err(refusal);
      return;
    }
    setBusy(true);
    try {
      const stored = await uploadImage(drive.termId, file);
      // The path is PASTED (never entered): the harness reads the file by path.
      const r = await sendText(drive, stored.path, { enter: false });
      if (r.kind === "sent") {
        onTyped?.();
        setChip(imageChip(stored.sha256, stored.mime));
        onCaption(CAPTION.pastedOnly);
      }
    } catch (e) {
      toast.err(e instanceof DriveError ? e.detail : "Could not send the picture");
    } finally {
      setBusy(false);
    }
  };

  // Buttons act on click but never take focus on press: the textarea keeps it
  // (and so the soft keyboard stays up).
  const keepFocus = {
    onPointerDown: (e: { preventDefault(): void }) => e.preventDefault(),
    onMouseDown: (e: { preventDefault(): void }) => e.preventDefault(),
  };

  const shownCaption = caption ?? (bound ? null : CAPTION.unbound);
  return (
    <form
      className="kb-drive__composer"
      data-testid="kb-drive-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <QuickReplies onSend={(t) => void send(t)} disabled={busy} />
      <textarea
        className="kb-drive__input"
        data-testid="kb-drive-input"
        aria-label="Message to the terminal"
        placeholder="Message"
        rows={2}
        enterKeyHint="send"
        autoCapitalize="sentences"
        autoCorrect="on"
        spellCheck
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // A hardware keyboard: Ctrl/Cmd+Enter sends. Plain Enter is a newline.
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <div className="kb-drive__composer-row">
        {chip !== null ? (
          <span className="kb-drive__chip" data-testid="kb-drive-chip">
            {chip}
          </span>
        ) : null}
        <button
          type="button"
          className="kb-drive__key"
          data-testid="kb-drive-attach"
          aria-label="Attach a picture"
          disabled={busy}
          {...keepFocus}
          onClick={() => fileRef.current?.click()}
        >
          <Icon.Paperclip />
        </button>
        <input
          ref={fileRef}
          type="file"
          hidden
          data-testid="kb-drive-file"
          accept={UPLOAD_TYPES.join(",")}
          onChange={(e) => {
            const f = e.target.files?.[0];
            // Reset so the same picture can be picked again.
            e.target.value = "";
            if (f) void attach(f);
          }}
        />
        <button
          type="submit"
          className="kb-drive__send"
          data-testid="kb-drive-send"
          disabled={busy}
          {...keepFocus}
        >
          Send
        </button>
      </div>
      {shownCaption !== null ? (
        <div className="kb-drive__caption" data-testid="kb-drive-caption" role="status">
          {shownCaption}
        </div>
      ) : null}
    </form>
  );
}
