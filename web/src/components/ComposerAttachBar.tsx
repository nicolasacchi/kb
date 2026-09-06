import { forwardRef, useImperativeHandle, useRef } from "react";
import type { ComposerAttachments } from "../hooks/useComposerAttachments";
import { isImageType } from "../lib/attachmentUrl";
import { Icon } from "./icons";

// Y-track — the attach affordance under a composer: a paperclip button that
// opens a (hidden) multi-file picker, plus chips for in-flight (⏳ / ⚠ retry)
// and staged (🖼/📎 + remove ×) uploads. Drag-drop + paste are wired by the
// composer container (CommentsPanel), which also owns the `onToken` insert.
//
// The editor's toolbar 📎 and slash `/attach` open the SAME picker via the
// imperative `open()` handle; pass `compact` to hide this bar's own button when
// the toolbar already provides the affordance (chips still render).

export type ComposerAttachBarHandle = {
  /// Open the hidden file picker (used by the editor toolbar / slash command).
  open: () => void;
};

type Props = {
  att: ComposerAttachments;
  /// Insert a markdown token into the draft at the cursor (the composer
  /// passes `editorRef.current?.insertAtCursor`).
  onToken: (token: string) => void;
  /// Hide this bar's own "📎 attach" button (the editor toolbar owns it).
  compact?: boolean;
};

const ComposerAttachBar = forwardRef<ComposerAttachBarHandle, Props>(
  function ComposerAttachBar({ att, onToken, compact }, ref) {
    const inputRef = useRef<HTMLInputElement | null>(null);
    const hasChips = att.pending.length > 0 || att.staged.length > 0;

    useImperativeHandle(ref, () => ({ open: () => inputRef.current?.click() }), []);

    return (
      <div className="cp__attachbar">
        {!compact && (
          <button
            type="button"
            className="cp__attach-btn"
            title="attach files or images"
            onClick={() => inputRef.current?.click()}
          >
            <Icon.Paperclip aria-hidden="true" /> attach
          </button>
        )}
        <input
          ref={inputRef}
          type="file"
          multiple
          data-testid="attach-input"
          className="cp__attach-input"
          onChange={(e) => {
            const files = Array.from(e.target.files ?? []);
            if (files.length) att.upload(files, onToken);
            // Reset so picking the same file again re-fires onChange.
            e.target.value = "";
          }}
        />
        {hasChips && (
          <div className="cp__attach-chips">
            {att.pending.map((p) => (
              <span
                key={p.key}
                className={`cp__attach-chip ${p.error ? "is-error" : "is-pending"}`}
                title={p.error ?? "uploading…"}
              >
                <span aria-hidden="true">{p.error ? "⚠" : "⏳"}</span>{" "}
                {p.filename}
                {p.error && (
                  <button
                    type="button"
                    className="cp__attach-retry"
                    onClick={() => att.retry(p, onToken)}
                  >
                    retry
                  </button>
                )}
              </span>
            ))}
            {att.staged.map((s) => (
              <span key={s.id} className="cp__attach-chip is-staged" title={s.filename}>
                <span aria-hidden="true">
                  {isImageType(s.contentType) ? <Icon.ImageFrame /> : <Icon.Paperclip />}
                </span>{" "}
                {s.filename}
                <button
                  type="button"
                  className="cp__attach-remove"
                  aria-label={`remove ${s.filename}`}
                  onClick={() => att.removeStaged(s.id)}
                >
                  <Icon.X />
                </button>
              </span>
            ))}
          </div>
        )}
      </div>
    );
  },
);

export default ComposerAttachBar;
