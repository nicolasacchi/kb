import { useEffect, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { Icon } from "../icons";
import type { ListEntry } from "../../api/client";
import { entryTrailHref } from "../../lib/listTrail";

// RL-track — one ordered entry on the list detail page.
//
//   #idx ○|◐|● [title → trail href] §section folder ~Nm ⚠stale
//        note (click-to-edit)                       [↑] [↓] [×]
//
// The read-dot shows the EFFECTIVE state (override ring when manual).
// Reorder surfaces three ways: the ↑/↓ buttons (primary, keyboard-
// accessible), Alt+↑/↓ on the focused row, and HTML5 drag-and-drop on
// the row (enhancement — no dnd library; lists are human-scale).

export const READ_DOT: Record<string, string> = {
  unread: "○",
  in_progress: "◐",
  read: "●",
};

function anchorChip(e: ListEntry): { label: string; title: string } | null {
  const a = e.anchor;
  if (!a) return null;
  if (a.kind === "section")
    return { label: `§${a.id}`, title: a.snippet ?? `section ${a.id}` };
  if (a.kind === "chapter") {
    const leaf = a.path.split(" > ").pop() ?? a.path;
    return { label: `§${leaf}`, title: a.path };
  }
  // W2.16 — a short excerpt reads better at a glance than the generic
  // "❝quote" this used to show for every selection entry; `.kb-listd__chip`
  // is `flex: none` (no CSS ellipsis truncation), so keep this short rather
  // than relying on layout to clip it.
  if (a.kind === "selection") {
    const excerpt = a.snippet.length > 24 ? `${a.snippet.slice(0, 24)}…` : a.snippet;
    return { label: `❝${excerpt}`, title: a.snippet };
  }
  return null;
}

export default function EntryRow({
  entry,
  kb,
  listId,
  onToggleRead,
  onNote,
  onMoveBy,
  onDropBefore,
  onRemove,
  onDragState,
}: {
  entry: ListEntry;
  kb: string;
  listId: string;
  onToggleRead: (e: ListEntry) => void;
  onNote: (eid: string, note: string | null) => void;
  onMoveBy: (eid: string, delta: -1 | 1) => void;
  onDropBefore: (draggedId: string, targetId: string) => void;
  onRemove: (eid: string) => void;
  onDragState: (dragging: string | null) => void;
}) {
  const [editingNote, setEditingNote] = useState(false);
  const [noteDraft, setNoteDraft] = useState(entry.note ?? "");
  const noteRef = useRef<HTMLTextAreaElement | null>(null);
  useEffect(() => {
    if (editingNote) noteRef.current?.focus();
  }, [editingNote]);

  const chip = anchorChip(entry);
  const href = entryTrailHref(kb, listId, entry);
  const dotTitle = entry.read_override
    ? `manually marked ${entry.read_override} — click to change`
    : `${entry.read_state.replace("_", " ")} (derived from your reading) — click to override`;

  const saveNote = () => {
    setEditingNote(false);
    const next = noteDraft.trim();
    if (next === (entry.note ?? "")) return;
    onNote(entry.id, next === "" ? null : next);
  };

  return (
    <li
      className={`kb-listd__row ${entry.tombstone ? "is-tombstone" : ""}`}
      data-testid="list-entry"
      data-entry-id={entry.id}
      tabIndex={0}
      draggable
      onDragStart={(ev) => {
        ev.dataTransfer.setData("text/kb-entry-id", entry.id);
        ev.dataTransfer.effectAllowed = "move";
        onDragState(entry.id);
      }}
      onDragEnd={() => onDragState(null)}
      onDragOver={(ev) => {
        if (ev.dataTransfer.types.includes("text/kb-entry-id"))
          ev.preventDefault();
      }}
      onDrop={(ev) => {
        const dragged = ev.dataTransfer.getData("text/kb-entry-id");
        if (dragged && dragged !== entry.id) {
          ev.preventDefault();
          onDropBefore(dragged, entry.id);
        }
        onDragState(null);
      }}
      onKeyDown={(ev) => {
        if (!ev.altKey || (ev.key !== "ArrowUp" && ev.key !== "ArrowDown"))
          return;
        ev.preventDefault();
        onMoveBy(entry.id, ev.key === "ArrowUp" ? -1 : 1);
      }}
    >
      <span className="kb-listd__idx">{entry.position + 1}</span>
      {/* P8 / invariant #25 — session entries have no "read" progress, so the
          read-state dot is suppressed (a neutral, non-interactive marker). */}
      {entry.is_session ? (
        <span
          className="kb-listd__dot is-session"
          title="session transcript — no read state"
          aria-label="session transcript"
        >
          ◆
        </span>
      ) : (
        <button
          type="button"
          className={`kb-listd__dot is-${entry.read_state} ${entry.read_override ? "is-override" : ""}`}
          onClick={() => onToggleRead(entry)}
          title={dotTitle}
          aria-label={`read state: ${entry.read_state}`}
        >
          {READ_DOT[entry.read_state] ?? "○"}
        </button>
      )}
      <div className="kb-listd__body">
        <div className="kb-listd__line">
          {href && !entry.tombstone ? (
            <Link className="kb-listd__title" to={href}>
              {entry.title ?? entry.source_relative}
            </Link>
          ) : (
            <span className="kb-listd__title is-dead" title="artifact removed from the kb">
              {entry.title ?? entry.source_relative ?? "(removed)"}
            </span>
          )}
          {chip && (
            <span className="kb-listd__chip" title={chip.title}>
              {chip.label}
            </span>
          )}
          {entry.folder ? (
            <span className="kb-listd__folder">{entry.folder}</span>
          ) : null}
          {entry.est_minutes != null && (
            <span className="kb-listd__min">~{entry.est_minutes}m</span>
          )}
          {entry.anchor_stale && (
            <span
              className="kb-listd__stale"
              title="the section this entry targets no longer resolves in the artifact — re-anchor via `kb list reanchor`"
            >
              ⚠ stale
            </span>
          )}
        </div>
        {editingNote ? (
          <textarea
            ref={noteRef}
            className="kb-listd__note-edit"
            rows={2}
            value={noteDraft}
            onChange={(ev) => setNoteDraft(ev.target.value)}
            onBlur={saveNote}
            onKeyDown={(ev) => {
              if (ev.key === "Enter" && (ev.metaKey || ev.ctrlKey)) saveNote();
              if (ev.key === "Escape") {
                setNoteDraft(entry.note ?? "");
                setEditingNote(false);
              }
            }}
            aria-label="entry note"
          />
        ) : (
          <button
            type="button"
            className={`kb-listd__note ${entry.note ? "" : "is-empty"}`}
            onClick={() => {
              setNoteDraft(entry.note ?? "");
              setEditingNote(true);
            }}
            title="edit note"
          >
            {entry.note ?? "add a note…"}
          </button>
        )}
      </div>
      <span className="kb-listd__acts">
        <button
          type="button"
          className="kb-listd__act"
          onClick={() => onMoveBy(entry.id, -1)}
          title="move up (Alt+↑)"
          aria-label="move up"
        >
          ↑
        </button>
        <button
          type="button"
          className="kb-listd__act"
          onClick={() => onMoveBy(entry.id, 1)}
          title="move down (Alt+↓)"
          aria-label="move down"
        >
          ↓
        </button>
        <button
          type="button"
          className="kb-listd__act kb-listd__act--rm"
          onClick={() => onRemove(entry.id)}
          title="remove from list"
          aria-label="remove entry"
        >
          <Icon.X />
        </button>
      </span>
    </li>
  );
}
