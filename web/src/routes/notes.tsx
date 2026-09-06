import { useMemo, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useNotes } from "../hooks/useNotes";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { useIdentity } from "../hooks/useArtifactHost";
import NoteEditor from "../components/NoteEditor";
import EmptyState from "../components/EmptyState";
import Loading from "../components/Loading";
import { Icon } from "../components/icons";
import NotesPanel, { NoteRow } from "../components/NotesPanel";
import type { NoteSummary } from "../api/notes";

// N-track — /notes. Free-standing notes / todo-lists across every kb,
// grouped by kb → folder (the scope notepad pinned first). Selecting a note
// opens it in the right rail (interactive checkboxes + inline editor). The
// rail falls back to the contextual "Notes for <scope>" panel so a "new
// note here" affordance is always one click away.

function scopeName(folder: string): string {
  return folder.length > 0 ? folder : "(root)";
}

export default function NotesRoute() {
  useDocumentTitle("Notes");
  const { notes, statuses, loading, error, create } = useNotes();
  const identity = useIdentity();
  const [params, setParams] = useSearchParams();

  const focus = params.get("focus"); // "<kb>:<id>"
  const activeKb = params.get("kb") || identity?.kbs?.[0] || null;
  const activeFolder = params.get("folder");

  const [statusFilter, setStatusFilter] = useState<string | null>(null);
  const [text, setText] = useState("");

  const open = (kb: string, id: string) => {
    const next = new URLSearchParams(params);
    next.set("focus", `${kb}:${id}`);
    setParams(next, { replace: false });
  };

  const visible = useMemo(() => {
    const q = text.trim().toLowerCase();
    return notes.filter((n) => {
      if (statusFilter && (n.status ?? "") !== statusFilter) return false;
      if (q && !`${n.title} ${n.folder}`.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [notes, statusFilter, text]);

  const visibleByScope = useMemo(() => {
    const map = new Map<string, Map<string, NoteSummary[]>>();
    for (const n of visible) {
      let folders = map.get(n.kb);
      if (!folders) {
        folders = new Map();
        map.set(n.kb, folders);
      }
      const list = folders.get(n.folder) ?? [];
      list.push(n);
      folders.set(n.folder, list);
    }
    for (const folders of map.values())
      for (const list of folders.values())
        list.sort((a, b) => {
          if (a.is_notepad !== b.is_notepad) return a.is_notepad ? -1 : 1;
          return (b.updated_at ?? 0) - (a.updated_at ?? 0);
        });
    return map;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [visible]);

  const newNote = async () => {
    if (!activeKb) return;
    await create(activeKb, {
      folder: activeFolder ?? "",
      title: "New note",
      body_md: "- [ ] ",
    });
  };

  const [focusKb, focusId] = focus ? focus.split(/:(.+)/) : [null, null];

  return (
    <div className="kb-notes" data-testid="notes-view">
      <header className="kb-notes__head">
        <h1 className="kb-notes__title">Notes</h1>
        <nav className="kb-notes__filters" role="group" aria-label="status filter">
          <button
            type="button"
            aria-pressed={statusFilter === null}
            className={`kb-notes__chip ${statusFilter === null ? "is-on" : ""}`}
            onClick={() => setStatusFilter(null)}
          >
            all
          </button>
          {statuses.map((s) => (
            <button
              key={s}
              type="button"
              aria-pressed={statusFilter === s}
              className={`kb-notes__chip ${statusFilter === s ? "is-on" : ""}`}
              onClick={() => setStatusFilter((cur) => (cur === s ? null : s))}
            >
              {s}
            </button>
          ))}
        </nav>
        <input
          type="search"
          className="kb-notes__search"
          placeholder="filter notes…"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <button
          type="button"
          className="kb-notes__new"
          onClick={newNote}
          disabled={!activeKb}
          title={activeKb ? `new note in ${activeKb}` : "no kb"}
        >
          + New note
        </button>
      </header>

      <div className="kb-notes__body">
        <section className="kb-notes__main">
          {loading && (
            <Loading icon={<Icon.Note />} variant="inline" label="loading notes…" />
          )}
          {error && <div className="kb-notes__empty">notes failed: {error}</div>}
          {!loading && !error && visible.length === 0 && (
            <EmptyState
              icon={<Icon.Note />}
              title="no notes yet"
              hint="Create one with + New note, the contextual panel while browsing a folder, or from the CLI."
              cli="kb notes new"
            />
          )}
          {Array.from(visibleByScope.entries()).map(([kb, folders]) => (
            <div key={kb} className="kb-notes__kb">
              <h2 className="kb-notes__kb-head">{kb}</h2>
              {Array.from(folders.entries())
                .sort((a, b) => a[0].localeCompare(b[0]))
                .map(([folder, list]) => (
                  <div key={folder} className="kb-notes__scope">
                    <h3 className="kb-notes__scope-head">{scopeName(folder)}</h3>
                    <ul className="kb-notes__list">
                      {list.map((n) => (
                        <li key={n.id}>
                          <NoteRow note={n} onOpen={() => open(n.kb, n.id)} />
                        </li>
                      ))}
                    </ul>
                  </div>
                ))}
            </div>
          ))}
        </section>

        <aside className="kb-notes__rail">
          {focusKb && focusId ? (
            <NoteEditor
              kb={focusKb}
              id={focusId}
              onDeleted={() => {
                const next = new URLSearchParams(params);
                next.delete("focus");
                setParams(next, { replace: true });
              }}
            />
          ) : (
            <NotesPanel kb={activeKb} folder={activeFolder} onOpenNote={open} />
          )}
        </aside>
      </div>
    </div>
  );
}
