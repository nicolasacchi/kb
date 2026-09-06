import { useState } from "react";
import { useNotes, useScopeNotes } from "../hooks/useNotes";
import type { NoteSummary } from "../api/notes";
import { Icon } from "./icons";

// N-track — contextual "Notes for <scope>" panel. Shown in the gallery
// right rail (scoped to the active folder filter) and reused in the /notes
// view. Surfaces the scope's canonical notepad (or a "Start notepad"
// affordance) + ad-hoc notes, each opening into the editor via `onOpenNote`.

type Props = {
  kb: string | null;
  /// null / "" = kb-root scope.
  folder: string | null;
  onOpenNote: (kb: string, id: string) => void;
};

function scopeLabel(kb: string | null, folder: string | null): string {
  if (!kb) return "—";
  return folder && folder.length > 0 ? folder : `${kb} (root)`;
}

export default function NotesPanel({ kb, folder, onOpenNote }: Props) {
  const { notepad, adhoc, loading } = useScopeNotes(kb, folder);
  const { create } = useNotes();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  if (!kb) {
    return (
      <aside className="kb-notes-panel">
        <div className="kb-notes-panel__empty">Pick a kb to see its notes.</div>
      </aside>
    );
  }

  const scope = folder ?? "";

  const startNotepad = async () => {
    setBusy(true);
    setErr(null);
    try {
      await create(kb, { folder: scope, notepad: true, body_md: "" });
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const newNote = async () => {
    setBusy(true);
    setErr(null);
    try {
      await create(kb, { folder: scope, title: "New note", body_md: "- [ ] " });
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="kb-notes-panel">
      <header className="kb-notes-panel__head">
        <span className="kb-notes-panel__title">Notes for</span>
        <span className="kb-notes-panel__scope">{scopeLabel(kb, folder)}</span>
      </header>

      {err && <div className="kb-notes-panel__err">{err}</div>}
      {loading && <div className="kb-notes-panel__empty">loading…</div>}

      <div className="kb-notes-panel__notepad">
        {notepad ? (
          <NoteRow note={notepad} onOpen={() => onOpenNote(kb, notepad.id)} />
        ) : (
          <button
            type="button"
            className="kb-notes-panel__start"
            onClick={startNotepad}
            disabled={busy}
          >
            <Icon.Plus aria-hidden="true" /> Start notepad for this scope
          </button>
        )}
      </div>

      {adhoc.length > 0 && (
        <ul className="kb-notes-panel__list">
          {adhoc.map((n) => (
            <li key={n.id}>
              <NoteRow note={n} onOpen={() => onOpenNote(kb, n.id)} />
            </li>
          ))}
        </ul>
      )}

      <button type="button" className="kb-notes-panel__new" onClick={newNote} disabled={busy}>
        <Icon.Plus aria-hidden="true" /> New note here
      </button>
    </aside>
  );
}

export function NoteRow({ note, onOpen }: { note: NoteSummary; onOpen: () => void }) {
  const prog = note.task_total > 0 ? `${note.task_done}/${note.task_total}` : "";
  return (
    <button type="button" className="kb-note-row" onClick={onOpen} data-note-id={note.id}>
      {note.is_notepad && (
        <span className="kb-note-row__pin" title="notepad">
          <Icon.Pin aria-hidden="true" />
        </span>
      )}
      <span className="kb-note-row__title">{note.title}</span>
      {prog && (
        <span
          className="kb-note-row__progress"
          data-done={note.task_done}
          data-total={note.task_total}
        >
          {prog}
        </span>
      )}
    </button>
  );
}
