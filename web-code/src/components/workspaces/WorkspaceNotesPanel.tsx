import { useState } from "react";
import type { AnnotationView } from "../../api/types";
import {
  useCreateWorkspaceNote,
  useDeleteWorkspaceNote,
  usePatchWorkspaceNote,
  useWorkspaceNotes,
} from "../../hooks/useWorkspaceNotes";
import {
  buildWorkspaceNotePayload,
  buildWorkspaceReplyPayload,
  groupWorkspaceNotes,
  type AnnotationThread,
} from "../../lib/workspaceNotes";
import { formatUnixSeconds } from "../../lib/format";
import { toast } from "../../lib/toast";
import { useConfirm } from "../ConfirmProvider";

export interface WorkspaceNotesPanelProps {
  repo: string;
  setId: string;
  /// The FOCUSED pane's current file/line, when one is open — prefills the
  /// composer's "attach to current line" option. `null` when no file is
  /// open (the composer then offers only a general note).
  currentPath: string | null;
  currentLine: number | null;
  /// Jump to a code-anchored note's location — may leave the current file
  /// (a workspace can span many files); `undefined` degrades the anchor
  /// chip to plain text (no jump affordance) for a host that hasn't wired
  /// cross-file navigation yet.
  onGotoNote?: (path: string, line: number) => void;
}

/// V70-A10 ("Workspaces v0", D26) — the rail's "Workspace notes" section: a
/// composer (general note, or "attach to current line" when a file is
/// open) plus the workspace's existing notes, general ones first, then
/// code-anchored ones grouped by file, each thread's replies expandable.
/// Notes are annotations (`set_id`-scoped) — they ride the SAME PATCH/
/// DELETE lifecycle and the SAME `annotation.changed` SSE carry-forward
/// every other annotation does; nothing here reimplements that.
export default function WorkspaceNotesPanel({
  repo,
  setId,
  currentPath,
  currentLine,
  onGotoNote,
}: WorkspaceNotesPanelProps) {
  const { data, isLoading } = useWorkspaceNotes(setId);
  const create = useCreateWorkspaceNote(setId);
  const patch = usePatchWorkspaceNote(setId);
  const del = useDeleteWorkspaceNote(setId);
  const confirm = useConfirm();

  const [body, setBody] = useState("");
  const [attachHere, setAttachHere] = useState(false);

  const canAttach = currentPath !== null && currentLine !== null;
  const payload = buildWorkspaceNotePayload({
    repo,
    setId,
    body,
    path: attachHere && canAttach ? (currentPath as string) : undefined,
    line: attachHere && canAttach ? (currentLine as number) : undefined,
  });

  async function save() {
    if (!payload) return;
    try {
      await create.mutateAsync(payload);
      setBody("");
      setAttachHere(false);
    } catch (e) {
      toast.err(`couldn't save note: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function toggleResolved(id: string, resolved: boolean) {
    try {
      await patch.mutateAsync({ id, input: { resolved: !resolved } });
    } catch (e) {
      toast.err(`couldn't update note: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function remove(id: string) {
    const ok = await confirm({
      title: "Delete this note?",
      body: "This can't be undone.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await del.mutateAsync(id);
    } catch (e) {
      toast.err(`couldn't delete note: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function reply(parentId: string, replyBody: string): Promise<boolean> {
    const replyPayload = buildWorkspaceReplyPayload(repo, parentId, replyBody);
    if (!replyPayload) return false;
    try {
      await create.mutateAsync(replyPayload);
      return true;
    } catch (e) {
      toast.err(`couldn't post reply: ${e instanceof Error ? e.message : String(e)}`);
      return false;
    }
  }

  if (isLoading) {
    return <div className="kbc-ws-notes__empty">Loading notes…</div>;
  }

  const groups = groupWorkspaceNotes(data?.annotations ?? []);
  const empty = groups.general.length === 0 && groups.byFile.length === 0;

  return (
    <div className="kbc-ws-notes" data-kbc-ws-notes={setId}>
      <div className="kbc-ws-notes__composer">
        <textarea
          className="kbc-ws-notes__body-input"
          placeholder="Add a workspace note…"
          value={body}
          onChange={(e) => setBody(e.target.value)}
          aria-label="workspace note body"
          data-kbc-ws-note-body
          rows={2}
        />
        {canAttach && (
          <label className="kbc-ws-notes__attach-toggle" title={`Attach to ${currentPath}:${currentLine}`}>
            <input
              type="checkbox"
              checked={attachHere}
              onChange={(e) => setAttachHere(e.target.checked)}
              data-kbc-ws-note-attach
            />
            Attach to {currentPath}:{currentLine}
          </label>
        )}
        <button
          type="button"
          className="kbc-ws-notes__save"
          disabled={!payload || create.isPending}
          onClick={() => void save()}
          data-kbc-ws-note-save
        >
          {create.isPending ? "Saving…" : "Save note"}
        </button>
      </div>

      {empty ? (
        <div className="kbc-ws-notes__empty">No notes on this workspace yet.</div>
      ) : (
        <>
          {groups.general.length > 0 && (
            <section className="kbc-ws-notes__section" data-kbc-ws-notes-general>
              <h4 className="kbc-ws-notes__section-head">General</h4>
              <ul className="kbc-ws-notes__list">
                {groups.general.map((t) => (
                  <WorkspaceNoteThread
                    key={t.parent.id}
                    thread={t}
                    onGoto={undefined}
                    onToggleResolved={toggleResolved}
                    onDelete={remove}
                    onReply={reply}
                  />
                ))}
              </ul>
            </section>
          )}
          {groups.byFile.map((g) => (
            <section className="kbc-ws-notes__section" key={g.path} data-kbc-ws-notes-file={g.path}>
              <h4 className="kbc-ws-notes__section-head" title={g.path}>
                {g.path}
              </h4>
              <ul className="kbc-ws-notes__list">
                {g.threads.map((t) => (
                  <WorkspaceNoteThread
                    key={t.parent.id}
                    thread={t}
                    onGoto={onGotoNote ? () => onGotoNote(g.path, t.parent.line) : undefined}
                    onToggleResolved={toggleResolved}
                    onDelete={remove}
                    onReply={reply}
                  />
                ))}
              </ul>
            </section>
          ))}
        </>
      )}
    </div>
  );
}

function WorkspaceNoteThread({
  thread,
  onGoto,
  onToggleResolved,
  onDelete,
  onReply,
}: {
  thread: AnnotationThread;
  onGoto: (() => void) | undefined;
  onToggleResolved: (id: string, resolved: boolean) => void;
  onDelete: (id: string) => void;
  onReply: (parentId: string, body: string) => Promise<boolean>;
}) {
  const { parent, replies } = thread;
  const [replyOpen, setReplyOpen] = useState(false);
  const [replyBody, setReplyBody] = useState("");
  const [replyPending, setReplyPending] = useState(false);

  async function saveReply() {
    if (!replyBody.trim() || replyPending) return;
    setReplyPending(true);
    const ok = await onReply(parent.id, replyBody);
    setReplyPending(false);
    if (ok) {
      setReplyBody("");
      setReplyOpen(false);
    }
  }

  return (
    <li
      className={"kbc-ws-notes__thread" + (parent.resolved ? " is-resolved" : "")}
      data-kbc-ws-note-id={parent.id}
    >
      <div className="kbc-ws-notes__item">
        {onGoto && (
          <button type="button" className="kbc-ws-notes__goto" onClick={onGoto} data-kbc-ws-note-goto>
            L{parent.line}
          </button>
        )}
        <div className="kbc-ws-notes__item-body">
          <p className={"kbc-ws-notes__text" + (parent.resolved ? " kbc-ws-notes__text--resolved" : "")}>
            {parent.body}
          </p>
          <div className="kbc-ws-notes__item-meta">
            <span>
              {parent.author} · {formatUnixSeconds(parent.updated_at)}
            </span>
            {parent.resolved && <span className="kbc-ws-notes__badge">resolved</span>}
          </div>
        </div>
        <div className="kbc-ws-notes__item-actions">
          <button
            type="button"
            onClick={() => onToggleResolved(parent.id, parent.resolved)}
            data-kbc-ws-note-resolve
          >
            {parent.resolved ? "Reopen" : "Resolve"}
          </button>
          <button type="button" onClick={() => setReplyOpen((v) => !v)} data-kbc-ws-note-reply-toggle>
            Reply{replies.length > 0 ? ` (${replies.length})` : ""}
          </button>
          <button
            type="button"
            className="kbc-ws-notes__delete"
            onClick={() => onDelete(parent.id)}
            data-kbc-ws-note-delete
          >
            Delete
          </button>
        </div>
      </div>

      {replies.length > 0 && (
        <ul className="kbc-ws-notes__replies" data-kbc-ws-note-replies={parent.id}>
          {replies.map((r: AnnotationView) => (
            <li key={r.id} className="kbc-ws-notes__reply" data-kbc-ws-note-reply-id={r.id}>
              <p className="kbc-ws-notes__text">{r.body}</p>
              <span className="kbc-ws-notes__reply-meta">
                {r.author} · {formatUnixSeconds(r.updated_at)}
              </span>
            </li>
          ))}
        </ul>
      )}

      {replyOpen && (
        <div className="kbc-ws-notes__reply-composer">
          <input
            type="text"
            className="kbc-ws-notes__reply-input"
            placeholder="Reply…"
            value={replyBody}
            onChange={(e) => setReplyBody(e.target.value)}
            aria-label="reply body"
            data-kbc-ws-note-reply-input
            onKeyDown={(e) => {
              if (e.key === "Enter") void saveReply();
            }}
          />
          <button
            type="button"
            disabled={!replyBody.trim() || replyPending}
            onClick={() => void saveReply()}
            data-kbc-ws-note-reply-save
          >
            {replyPending ? "Posting…" : "Post"}
          </button>
        </div>
      )}
    </li>
  );
}
