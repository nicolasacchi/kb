// A node's THREAD (V74-L2, D10: "node threads reuse the annotations store").
//
// There is no second comments table and no board-specific comment route: a
// node's `thread_id` is an `annotations.id`, and everything below goes through
// the existing `POST /api/annotations` + `lib/annotations.ts` builders. Two
// consequences worth stating:
//
// **The first reply CREATES the parent, and the board records it.** A node with
// no thread yet has nothing to reply to, so the first comment is an ordinary
// annotation at the node's own anchor; the board then stores its id through
// `apply` (loopback-only). If that second write is refused, the comment is
// still there — the panel says only the LINK could not be recorded, because
// losing the comment to a failed bookkeeping write would be far worse.
//
// **A thread anchors where the node points, or not at all.** An annotation
// needs a path AND a line (`routes::CreateAnnotationBody`), which a `note` or a
// `turn` node does not have. `threadAnchorFor` returns `null` there and the
// affordance says why rather than inventing a path so a button could be
// enabled.

import { useMemo, useState, type FormEvent } from "react";
import type { BoardNode, BoardOut } from "../../api/types";
import { ApiError, createAnnotation } from "../../api/client";
import { useAnnotations } from "../../hooks/useAnnotations";
import { useApplyBoard } from "../../hooks/useBoards";
import { buildCreatePayload, buildReplyPayload, groupThreads } from "../../lib/annotations";
import { composeThread, threadAnchorFor } from "../../lib/boardDoc";
import { toast } from "../../lib/toast";
import { Icon } from "../icons";

export interface BoardThreadPanelProps {
  board: BoardOut;
  node: BoardNode;
  onClose(): void;
}

export default function BoardThreadPanel({ board, node, onClose }: BoardThreadPanelProps) {
  const anchor = threadAnchorFor(node);
  const annotations = useAnnotations(board.repo, anchor?.path);
  const apply = useApplyBoard(board.repo);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const thread = useMemo(() => {
    if (!node.thread) return null;
    const rows = annotations.data?.annotations ?? [];
    return groupThreads(rows).find((t) => t.parent.id === node.thread?.id) ?? null;
  }, [annotations.data, node.thread]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!anchor || !draft.trim()) return;
    setBusy(true);
    setError(null);
    try {
      if (node.thread?.id) {
        const payload = buildReplyPayload(board.repo, anchor.path, node.thread.id, draft);
        if (!payload) throw new Error("empty reply");
        await createAnnotation(payload);
      } else {
        const payload = buildCreatePayload({
          repo: board.repo,
          path: anchor.path,
          line: anchor.line,
          lineEnd: anchor.lineEnd,
          body: draft,
          anchorKind: anchor.lineEnd ? "range" : "line",
          intent: "note",
        });
        if (!payload) throw new Error("empty comment");
        const created = await createAnnotation(payload);
        try {
          const composed = composeThread(board, node.id, created.id);
          await apply.mutateAsync({ doc: composed.doc });
        } catch (linkErr) {
          const why = linkErr instanceof ApiError ? linkErr.message : String(linkErr);
          toast.warn(
            `the comment was saved, but the board could not record it as this node's thread (${why}) — recording a thread writes through the loopback-only apply`,
          );
        }
      }
      setDraft("");
      void annotations.refetch();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="kbc-boardthread" data-kbc-board-thread-panel={node.id}>
      <header className="kbc-boardthread__head">
        <Icon.Comment />
        <span>Thread — {node.title ?? node.id}</span>
        <button type="button" onClick={onClose} aria-label="close thread" data-kbc-board-thread-close>
          <Icon.X />
        </button>
      </header>

      {!anchor ? (
        <p className="kbc-boardthread__why" data-kbc-board-thread-unavailable>
          A thread anchors at a path and a line. A <code>{node.kind}</code> node addresses neither,
          so there is nowhere honest to put one — the annotations store is not given a made-up
          position so this panel could look complete.
        </p>
      ) : (
        <>
          <p className="kbc-boardthread__anchor" data-kbc-board-thread-anchor>
            {anchor.path}:{anchor.line}
            {anchor.lineEnd ? `-${anchor.lineEnd}` : ""}
            {node.thread ? (
              <span data-kbc-board-thread-count>
                {" · "}
                {node.thread.replies} repl{node.thread.replies === 1 ? "y" : "ies"}
                {node.thread.resolved ? " · resolved" : ""}
              </span>
            ) : (
              <span> · no thread yet</span>
            )}
          </p>

          {thread ? (
            <ul className="kbc-boardthread__rows">
              <li className="kbc-boardthread__row" data-kbc-board-thread-parent={thread.parent.id}>
                <span className="kbc-boardthread__author">{thread.parent.author}</span>
                <span className="kbc-boardthread__body">{thread.parent.body}</span>
              </li>
              {thread.replies.map((r) => (
                <li className="kbc-boardthread__row" key={r.id} data-kbc-board-thread-reply={r.id}>
                  <span className="kbc-boardthread__author">{r.author}</span>
                  <span className="kbc-boardthread__body">{r.body}</span>
                </li>
              ))}
            </ul>
          ) : node.thread ? (
            <p className="kbc-boardthread__why">
              This node names a thread the annotations at <code>{anchor.path}</code> no longer
              carry — the comment was deleted, or it lives at a different path. Shown rather than
              hidden.
            </p>
          ) : null}

          <form className="kbc-boardthread__composer" onSubmit={(e) => void submit(e)}>
            <textarea
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder={node.thread ? "Reply…" : "Start this node's thread…"}
              aria-label="thread comment"
              rows={3}
              data-kbc-board-thread-input
            />
            <button type="submit" disabled={busy || !draft.trim()} data-kbc-board-thread-submit>
              {busy ? "Saving…" : node.thread ? "Reply" : "Comment"}
            </button>
          </form>
          {error && (
            <p className="kbc-boardthread__error" data-kbc-board-thread-error>
              {error}
            </p>
          )}
        </>
      )}
    </section>
  );
}
