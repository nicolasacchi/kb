// PQUI — the Now tab's prompt queue: the project's saved to-do prompts, kept by
// the daemon (so every device sees the same list). Add, edit, reorder, delete,
// mark done, and Send = paste into THIS terminal WITHOUT Enter, then mark done.
// Nothing here ever presses Enter and nothing sends on its own. Every prompt is
// a text node (no raw-HTML injection API) and its text never reaches a URL, a
// log line or a toast. The export name and `data-testid="now-queue"` /
// `data-seam="pqui"` are the place RAILNOW reserved in the Now column.

import { useState, type ReactElement } from "react";
import { StateCard } from "@kb/ui/StateCard";
import { useConfirm } from "../../../components/ConfirmProvider";
import { useLiveSessions } from "../../../hooks/useLiveSessions";
import { usePhonePanel } from "../../../hooks/useIsMobile";
import { useSessionDetail } from "../../../hooks/useSessions";
import type { Drive } from "../../phone/driveApi";
import { PromptsApiError, type PromptItem } from "../../prompts/promptsApi";
import { byteLength, COUNT_NEAR_BYTES, MAX_PROMPT_BYTES, moveTarget, splitItems, textOk, validProject } from "../../prompts/promptsModel";
import { FLATTENED_CAPTION, MARK_FAILED_CAPTION, sendPrompt } from "../../prompts/sendPrompt";
import { usePromptActions, useTermPrompts } from "../../prompts/usePrompts";
import { useTermRail } from "../context";
import "./nowRail.css";
import "./nowQueue.css";

export const STALE_CAPTION = "The queue changed on another device; it was reloaded. Try again.";
export const BLOCKED_CAPTION = "Session is waiting on you; answer it first";
export const PASTED_CAPTION = "Pasted into the terminal. Press Enter there to send it.";

export function NowQueueSeam(): ReactElement {
  const { sid } = useTermRail();
  const { detail } = useSessionDetail(sid ?? null);
  const project = sid ? validProject(detail?.project_slug) : null;
  return (
    <section className="kb-nowrail__section kb-nowq" data-testid="now-queue" data-seam="pqui" aria-label="Prompt queue">
      <header className="kb-nowrail__sechead">
        <h3 className="kb-nowrail__h">Prompt queue</h3>
        <span className="kb-nowrail__fresh" data-testid="now-fresh-queue">
          synced by the daemon
        </span>
      </header>
      {project === null ? (
        <p className="kb-nowrail__none" data-testid="now-queue-unbound">
          Link this terminal to a project to use the queue
        </p>
      ) : (
        <Queue project={project} />
      )}
    </section>
  );
}

function Queue({ project }: { project: string }): ReactElement {
  const { termId, harness, sid } = useTermRail();
  const confirm = useConfirm();
  const phone = usePhonePanel();
  const { rows: live } = useLiveSessions();
  const blocked = sid ? (live.find((r) => r.session_id === sid)?.blocked ?? false) : false;
  const q = useTermPrompts(project);
  const act = usePromptActions(project);
  const { open, done } = splitItems(q.data);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);

  // One in-flight request at a time: every control is disabled while it runs.
  async function run(fn: () => Promise<void>): Promise<boolean> {
    if (busy) return false;
    setBusy(true);
    setNote(null);
    try {
      await fn();
      return true;
    } catch (e) {
      if (e instanceof PromptsApiError && e.stale) {
        setNote(STALE_CAPTION);
        void act.refetch().catch(() => undefined);
      } else {
        setNote(e instanceof Error ? e.message : "Request failed");
      }
      return false;
    } finally {
      setBusy(false);
    }
  }

  const addNow = async (): Promise<void> => {
    if (!textOk(draft)) return;
    if (await run(async () => void (await act.add(draft)))) setDraft("");
  };

  const send = async (item: PromptItem): Promise<void> => {
    if (busy || blocked) return;
    setBusy(true);
    setNote(null);
    try {
      const drive: Drive = { termId, harness, viewer: null, confirm };
      const r = await sendPrompt({
        drive,
        item,
        markDone: (id) => act.patch(id, { done: true }),
        refetch: act.refetch,
      });
      if (r.kind === "error") setNote(r.detail);
      else if (r.kind === "sent") setNote(!r.marked ? MARK_FAILED_CAPTION : r.flattened ? FLATTENED_CAPTION : PASTED_CAPTION);
    } finally {
      setBusy(false);
    }
  };

  const remove = async (item: PromptItem): Promise<void> => {
    const ok = await confirm({
      title: "Delete this prompt?",
      body: "It is removed from this project's queue on every device.",
      confirmLabel: "Delete",
      danger: true,
    });
    if (ok) await run(async () => void (await act.remove(item.id)));
  };

  const move = (item: PromptItem, dir: -1 | 1): void => {
    const list = q.data;
    const position = list ? moveTarget(list, item.id, dir) : null;
    if (position !== null) void run(async () => void (await act.patch(item.id, { position })));
  };

  const saveEdit = async (): Promise<void> => {
    if (editing === null || !textOk(editing.text)) return;
    const { id, text } = editing;
    if (await run(async () => void (await act.patch(id, { text })))) setEditing(null);
  };

  const bytes = byteLength(draft.trim());
  const body = q.data;

  return (
    <div className="kb-nowq__body" data-testid="now-queue-body" data-phone={phone ? "1" : "0"}>
      <form
        className="kb-nowq__add"
        onSubmit={(ev) => {
          ev.preventDefault();
          void addNow();
        }}
      >
        <textarea
          className="kb-nowq__input"
          aria-label="New prompt"
          placeholder="Save a prompt for later"
          rows={2}
          value={draft}
          onChange={(ev) => setDraft(ev.target.value)}
          data-testid="queue-input"
        />
        <div className="kb-nowq__addrow">
          {bytes >= COUNT_NEAR_BYTES ? (
            <span className="kb-nowrail__fresh" data-testid="queue-count" data-over={bytes > MAX_PROMPT_BYTES ? "1" : "0"}>
              {bytes} / {MAX_PROMPT_BYTES} bytes
            </span>
          ) : null}
          <button type="submit" className="kb-nowq__btn" data-testid="queue-add" disabled={busy || !textOk(draft)}>
            Add
          </button>
        </div>
      </form>

      {note !== null ? (
        <p className="kb-nowq__note" role="status" data-testid="queue-note">
          {note}
        </p>
      ) : null}
      {blocked ? (
        <p className="kb-nowrail__muted" data-testid="queue-blocked">
          {BLOCKED_CAPTION}
        </p>
      ) : null}

      {q.isError && body === undefined ? (
        <StateCard kind="degraded" size="inline" title="Could not read the queue" reason={q.error instanceof Error ? q.error.message : undefined} />
      ) : body === undefined ? (
        <p className="kb-nowrail__none">Loading the queue</p>
      ) : body.items.length === 0 ? (
        <StateCard kind="empty" size="inline" title="No saved prompts" reason="Add one above; Send pastes it here without pressing Enter." />
      ) : (
        <>
          <ol className="kb-nowq__list" data-testid="queue-list">
            {open.map((it, i) => (
              <li key={it.id} className="kb-nowq__item" data-testid="queue-item" data-id={it.id}>
                {editing?.id === it.id ? (
                  <div className="kb-nowq__edit">
                    <textarea
                      className="kb-nowq__input"
                      aria-label="Edit prompt"
                      rows={3}
                      value={editing.text}
                      onChange={(ev) => setEditing({ id: it.id, text: ev.target.value })}
                      data-testid="queue-edit-input"
                    />
                    <div className="kb-nowq__actions">
                      <button type="button" className="kb-nowq__btn" data-testid="queue-save" disabled={busy || !textOk(editing.text)} onClick={() => void saveEdit()}>
                        Save
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-cancel" onClick={() => setEditing(null)}>
                        Cancel
                      </button>
                    </div>
                  </div>
                ) : (
                  <>
                    <p className="kb-nowq__text" data-testid="queue-text">
                      {it.text}
                    </p>
                    <div className="kb-nowq__actions">
                      <button type="button" className="kb-nowq__btn kb-nowq__btn--go" data-testid="queue-send" disabled={busy || blocked} onClick={() => void send(it)}>
                        Send
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-edit" disabled={busy} onClick={() => setEditing({ id: it.id, text: it.text })}>
                        Edit
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-up" aria-label="Move up" disabled={busy || i === 0} onClick={() => move(it, -1)}>
                        Up
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-down" aria-label="Move down" disabled={busy || i === open.length - 1} onClick={() => move(it, 1)}>
                        Down
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-delete" disabled={busy} onClick={() => void remove(it)}>
                        Delete
                      </button>
                    </div>
                  </>
                )}
              </li>
            ))}
          </ol>
          {done.length > 0 ? (
            <details className="kb-nowq__done" data-testid="queue-done">
              <summary className="kb-nowq__sum">Done ({done.length})</summary>
              <ol className="kb-nowq__list">
                {done.map((it) => (
                  <li key={it.id} className="kb-nowq__item kb-nowq__item--done" data-testid="queue-done-item" data-id={it.id}>
                    <p className="kb-nowq__text" data-testid="queue-done-text">
                      {it.text}
                    </p>
                    <div className="kb-nowq__actions">
                      <button type="button" className="kb-nowq__btn" data-testid="queue-reopen" disabled={busy} onClick={() => void run(async () => void (await act.patch(it.id, { done: false })))}>
                        Reopen
                      </button>
                      <button type="button" className="kb-nowq__btn" data-testid="queue-done-delete" disabled={busy} onClick={() => void remove(it)}>
                        Delete
                      </button>
                    </div>
                  </li>
                ))}
              </ol>
            </details>
          ) : null}
        </>
      )}
    </div>
  );
}
