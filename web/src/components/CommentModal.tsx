import { useEffect, useRef, useState } from "react";
import type { Comment } from "../api/client";
import { Icon } from "./icons";
import CommentBody from "./CommentBody";
import AttachmentStrip from "./AttachmentStrip";
import UserChip from "./UserChip";
import MarkdownEditor, { type MarkdownEditorHandle } from "./LazyMarkdownEditor";
import { CANNED, QuickButtons, threadAwaitsResponse } from "./QuickButtons";
import { useConfirm } from "./ConfirmProvider";
import { useIdentity } from "../hooks/useArtifactHost";
import { canEditComment } from "../lib/canEditComment";
import { relTime, anchorLabel } from "../lib/commentFmt";
import { useDraft } from "../lib/drafts";

// Expanded "bigger view" of a single comment: read the full markdown,
// edit your own body (live preview), quote it into a reply, and see the
// whole reply thread. Opened from a CommentRow's ⤢ expand action.
//
// Uses a native <dialog> via .showModal() (focus trap + ::backdrop +
// Escape) — the annotator precedent — NOT the export modal's .show().
export default function CommentModal({
  kb,
  artifactId,
  comment,
  onClose,
  onEditBody,
  onReply,
  onChoose,
  onOpenImage,
}: {
  kb: string;
  artifactId: string;
  comment: Comment;
  onClose: () => void;
  /// Persist an edit to this comment's body (own comments only).
  onEditBody: (body: string) => void;
  onReply: (author: "you" | "claude", body: string) => void;
  /// R3 — a quick-response button click: post `body` as a "you" reply,
  /// resolving the comment when `resolve` is set.
  onChoose: (body: string, resolve: boolean) => void;
  /// Y-track — open an inline/strip image in the lightbox.
  onOpenImage?: (url: string, alt: string) => void;
}) {
  const ref = useRef<HTMLDialogElement | null>(null);
  const editorRef = useRef<MarkdownEditorHandle | null>(null);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(comment.body);
  // A-SPA — same durable-draft slot (`reply:<commentId>`) as CommentRow's
  // inline reply composer for this comment (lib/drafts.ts): one logical
  // reply draft, so switching between the row's box and this modal never
  // orphans half-typed text.
  const replyDraft = useDraft(kb, artifactId, `reply:${comment.id}`);
  const replyBody = replyDraft.text;
  const setReplyBody = replyDraft.setText;
  const [replyAuthor, setReplyAuthor] = useState<"you" | "claude">("you");
  const confirm = useConfirm();
  const identity = useIdentity();
  const me = identity?.user;
  // v0.34 W — ownership is the server-stamped `user`, not the you|claude
  // role; legacy no-user rows belong to the CONFIGURED operator.
  const editable = canEditComment(comment, me, identity?.operator);

  // Open as a true modal on mount; restore focus to the trigger (the ⤢
  // button, which had focus when we mounted) on unmount.
  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const dlg = ref.current;
    if (dlg && !dlg.open) dlg.showModal();
    return () => trigger?.focus?.();
  }, []);

  // Route Escape through onClose so the parent clears its modal state (and
  // our unmount cleanup restores focus). We listen on BOTH the dialog's
  // native `cancel` event AND a keydown for Escape: a synthesized Escape
  // (Playwright / headless Chromium) doesn't reliably fire `cancel`, so the
  // keydown path guarantees the close. Both call `onClose`, which is
  // idempotent (it just nulls the parent's modalId).
  useEffect(() => {
    const dlg = ref.current;
    if (!dlg) return;
    const close = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close(e);
    };
    dlg.addEventListener("cancel", close);
    dlg.addEventListener("keydown", onKey);
    return () => {
      dlg.removeEventListener("cancel", close);
      dlg.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  // If the comment changes underneath us (live SSE refetch) and we're
  // not actively editing, follow the new body.
  useEffect(() => {
    if (!editing) setDraft(comment.body);
  }, [comment.body, editing]);

  function quote(text: string) {
    const t = text.trim();
    if (!t) return;
    const block = t
      .split("\n")
      .map((l) => `> ${l}`)
      .join("\n");
    setReplyBody(replyBody ? `${replyBody}\n\n${block}\n\n` : `${block}\n\n`);
    editorRef.current?.focusWrite();
  }

  // Capture the live selection in the mousedown handler — a button's
  // default mousedown moves focus and collapses the selection before a
  // click handler could read it. preventDefault keeps it alive.
  function quoteSelection(e: React.MouseEvent) {
    e.preventDefault();
    quote(window.getSelection()?.toString() ?? "");
  }

  function saveEdit() {
    const t = draft.trim();
    if (!t) return;
    onEditBody(t);
    setEditing(false);
  }

  // Leave edit mode, confirming first if the body has unsaved changes.
  async function cancelEdit() {
    if (
      draft.trim() !== comment.body.trim() &&
      !(await confirm({
        title: "Discard your edits?",
        body: "Your unsaved changes to this comment will be lost.",
        confirmLabel: "Discard",
      }))
    )
      return;
    setEditing(false);
    setDraft(comment.body);
  }

  function sendReply() {
    const t = replyBody.trim();
    if (!t) return;
    onReply(replyAuthor, t);
    replyDraft.clear();
  }

  return (
    <dialog
      ref={ref}
      className="cp__modal"
      aria-label="comment detail"
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
    >
      <div className="cp__modal-inner">
        <header className="cp__modal-head">
          <div className="cp__row-meta">
            <span className="cp__row-author">{comment.author}</span>
            <UserChip user={comment.user} me={me} className="cp__row-user" />
            <span className="cp__row-time">{relTime(comment.createdAt)}</span>
            {comment.editedAt && <span className="cp__row-edited">edited</span>}
            <span className="cp__row-anchor">{anchorLabel(comment)}</span>
          </div>
          <button onClick={onClose} aria-label="close" className="cp__modal-x">
            <Icon.X />
          </button>
        </header>

        <div className="cp__modal-body">
          {editing ? (
            <MarkdownEditor
              value={draft}
              onChange={setDraft}
              ariaLabel="edit comment body"
              autoFocus
              attachment={{ kb, id: artifactId }}
              onSubmit={saveEdit}
              renderPreview={(v) => (
                <CommentBody body={v} kb={kb} id={artifactId} />
              )}
            />
          ) : (
            <>
              <CommentBody
                body={comment.body}
                className="cp__modal-md"
                kb={kb}
                id={artifactId}
                onOpenImage={onOpenImage}
              />
              <AttachmentStrip
                kb={kb}
                id={artifactId}
                attachments={comment.attachments}
                onOpenImage={onOpenImage}
              />
            </>
          )}
        </div>

        {!editing &&
          comment.status === "open" &&
          comment.author === "claude" &&
          !!comment.choices?.length && (
            <div className="cp__modal-quick">
              <QuickButtons items={comment.choices} onChoose={onChoose} />
            </div>
          )}

        {!editing && (
          <div className="cp__modal-quotebar">
            <button onMouseDown={quoteSelection} title="quote the selected text into a reply">
              ❝ quote selection
            </button>
            <button onClick={() => quote(comment.body)} title="quote the whole comment">
              ❝ quote all
            </button>
          </div>
        )}

        {comment.replies.length > 0 && (
          <div className="cp__modal-replies">
            {comment.replies.map((r) => (
              <div key={r.id} className={`cp__reply cp__reply--${r.author}`}>
                <div className="cp__row-meta">
                  <span className="cp__row-author">{r.author}</span>
                  <UserChip user={r.user} me={me} className="cp__row-user" />
                  <span className="cp__row-time">{relTime(r.createdAt)}</span>
                </div>
                <CommentBody
                  body={r.body}
                  kb={kb}
                  id={artifactId}
                  onOpenImage={onOpenImage}
                />
                <AttachmentStrip
                  kb={kb}
                  id={artifactId}
                  attachments={r.attachments}
                  onOpenImage={onOpenImage}
                />
                {comment.status === "open" &&
                  r.author === "claude" &&
                  !!r.choices?.length && (
                    <QuickButtons items={r.choices} onChoose={onChoose} />
                  )}
              </div>
            ))}
          </div>
        )}

        {comment.status === "open" && threadAwaitsResponse(comment) && (
          <div className="cp__modal-quick">
            <QuickButtons items={CANNED} onChoose={onChoose} />
          </div>
        )}

        <div className="cp__reply-composer">
          <div
            className="cp__author-toggle"
            role="radiogroup"
            aria-label="reply author"
          >
            {(["you", "claude"] as const).map((a) => (
              <button
                key={a}
                role="radio"
                aria-checked={replyAuthor === a}
                className={`cp__author-btn ${replyAuthor === a ? "is-active" : ""}`}
                onClick={() => setReplyAuthor(a)}
              >
                {a}
              </button>
            ))}
          </div>
          <MarkdownEditor
            ref={editorRef}
            value={replyBody}
            onChange={setReplyBody}
            placeholder="reply… (markdown supported)"
            ariaLabel="reply body"
            textareaClassName="cp__reply-input"
            attachment={{ kb, id: artifactId }}
            onSubmit={sendReply}
            renderPreview={(v) => (
              <CommentBody body={v} kb={kb} id={artifactId} />
            )}
          />
          <button
            disabled={!replyBody.trim()}
            onClick={sendReply}
            className="cp__reply-send"
          >
            send reply
          </button>
        </div>

        <footer className="cp__modal-foot">
          {editable && !editing && (
            <button onClick={() => setEditing(true)}>✎ edit</button>
          )}
          {editing && (
            <>
              <button onClick={cancelEdit}>cancel</button>
              <button
                className="cp__modal-save"
                disabled={!draft.trim()}
                onClick={saveEdit}
              >
                save
              </button>
            </>
          )}
        </footer>
      </div>
    </dialog>
  );
}
