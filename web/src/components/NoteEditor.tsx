import { useEffect, useState } from "react";
import { useNote } from "../hooks/useNotes";
import MarkdownEditor from "./LazyMarkdownEditor";
import NoteMarkdown from "./NoteMarkdown";
import BacklinksSection from "./BacklinksSection";
import { useConfirm } from "./ConfirmProvider";
import { Icon } from "./icons";

// N-track — full interactive view + inline editor for one note. Renders the
// body with clickable checkboxes (toggling persists), a quick "add task"
// input, and an Edit mode (title + Markdown body via MarkdownEditor, whose
// Preview shows the rendered checklist). Used by the /notes rail, the
// contextual gallery panel, and the Detail view (native note render).

type Props = {
  kb: string;
  id: string;
  /// Called after a successful delete so the parent can clear its selection.
  onDeleted?: () => void;
};

function progressText(done: number, total: number): string {
  return total > 0 ? `${done}/${total}` : "";
}

export default function NoteEditor({ kb, id, onDeleted }: Props) {
  const { note, loading, error, setDirty, save, toggleTask, appendTask, remove } =
    useNote(kb, id);
  const [editing, setEditing] = useState(false);
  const [draftBody, setDraftBody] = useState("");
  const [draftTitle, setDraftTitle] = useState("");
  const [newTask, setNewTask] = useState("");
  const [busy, setBusy] = useState(false);
  const [actionErr, setActionErr] = useState<string | null>(null);
  const confirm = useConfirm();

  // Leaving edit mode (or switching notes) clears the dirty guard.
  useEffect(() => {
    setEditing(false);
    setDirty(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kb, id]);

  if (loading) return <div className="kb-note__empty">loading note…</div>;
  if (error) return <div className="kb-note__empty">note failed: {error}</div>;
  if (!note) return <div className="kb-note__empty">note not found</div>;

  const startEdit = () => {
    setDraftTitle(note.title);
    setDraftBody(note.body_md);
    setEditing(true);
    setDirty(true);
    setActionErr(null);
  };

  const cancelEdit = () => {
    setEditing(false);
    setDirty(false);
  };

  const onSave = async () => {
    setBusy(true);
    setActionErr(null);
    try {
      await save({ title: draftTitle, body_md: draftBody });
      setEditing(false);
      setDirty(false);
    } catch (e) {
      setActionErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onToggle = (index: number, on: boolean) => {
    toggleTask(index, on).catch((e) => setActionErr(String(e)));
  };

  const onAddTask = async () => {
    const text = newTask.trim();
    if (!text) return;
    setBusy(true);
    try {
      await appendTask(text);
      setNewTask("");
    } catch (e) {
      setActionErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async () => {
    const ok = await confirm({
      title: "Delete this note?",
      body: `Delete note “${note.title}”? This removes the file and can't be undone.`,
      confirmLabel: "Delete note",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await remove();
      onDeleted?.();
    } catch (e) {
      setActionErr(String(e));
      setBusy(false);
    }
  };

  const prog = progressText(note.task_done, note.task_total);

  return (
    <div className="kb-note" data-note-id={note.id}>
      <header className="kb-note__head">
        {editing ? (
          <input
            className="kb-note__title-input"
            value={draftTitle}
            onChange={(e) => setDraftTitle(e.target.value)}
            aria-label="note title"
            placeholder="Untitled note"
          />
        ) : (
          <h2 className="kb-note__title">
            {note.is_notepad && (
              <span className="kb-note__pin" title="scope notepad">
                <Icon.Pin aria-hidden="true" />
              </span>
            )}
            {note.title}
          </h2>
        )}
        <div className="kb-note__meta">
          {note.status && <span className="kb-note__status">{note.status}</span>}
          {note.comment_count > 0 && (
            <span
              className="kb-note__comments"
              title="comment threads — open via “View rendered” or `kb comments`"
            >
              <Icon.Comment aria-hidden="true" /> {note.comment_count}
            </span>
          )}
          {prog && (
            <span
              className="kb-note__progress"
              data-done={note.task_done}
              data-total={note.task_total}
              role="progressbar"
              aria-valuenow={note.task_done}
              aria-valuemax={note.task_total}
              aria-valuetext={`${prog} tasks done`}
            >
              <span
                className="kb-note__progress-fill"
                style={{
                  width: `${note.task_total > 0 ? (100 * note.task_done) / note.task_total : 0}%`,
                }}
              />
              <span className="kb-note__progress-label">{prog}</span>
            </span>
          )}
        </div>
        <div className="kb-note__actions">
          {editing ? (
            <>
              <button type="button" className="kb-note__btn" onClick={onSave} disabled={busy}>
                Save
              </button>
              <button type="button" className="kb-note__btn" onClick={cancelEdit} disabled={busy}>
                Cancel
              </button>
            </>
          ) : (
            <>
              <button type="button" className="kb-note__btn" onClick={startEdit}>
                Edit
              </button>
              <button
                type="button"
                className="kb-note__btn kb-note__btn--danger"
                onClick={onDelete}
                disabled={busy}
              >
                Delete
              </button>
            </>
          )}
        </div>
      </header>

      {actionErr && <div className="kb-note__err">{actionErr}</div>}

      {editing ? (
        <MarkdownEditor
          value={draftBody}
          onChange={setDraftBody}
          ariaLabel="note body"
          placeholder={
            "Write the note in Markdown.\nTodo items: - [ ] do a thing\nLink anything: [[title]]"
          }
          textareaClassName="kb-note__editor-input"
          wikilinkKb={kb}
          onSubmit={() => void onSave()}
          renderPreview={(v) => <NoteMarkdown body={v} links={note.links} />}
        />
      ) : (
        <>
          <NoteMarkdown body={note.body_md} onToggle={onToggle} links={note.links} />
          <form
            className="kb-note__add"
            onSubmit={(e) => {
              e.preventDefault();
              onAddTask();
            }}
          >
            <input
              className="kb-note__add-input"
              value={newTask}
              onChange={(e) => setNewTask(e.target.value)}
              placeholder="add a task…"
              aria-label="add a task"
            />
            <button type="submit" className="kb-note__btn" disabled={busy || !newTask.trim()}>
              Add
            </button>
          </form>
          <BacklinksSection kb={kb} id={id} />
        </>
      )}
    </div>
  );
}
