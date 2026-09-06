import { useMemo, useState } from "react";
import type { Bookmark } from "../../api/types";
import { useConfirm } from "../ConfirmProvider";
import {
  useBookmarks,
  useDeleteBookmark,
  usePatchBookmark,
} from "../../hooks/useBookmarks";
import { highlightSegments, speedFilterItems } from "../../lib/speedSearch";
import { toast } from "../../lib/toast";

export interface BookmarksPanelProps {
  repo: string;
  onJump: (loc: { path: string; line: number }) => void;
}

const MNEMONIC_RE = /^[0-9a-z]$/;

/// V3.N2 — InspectorRail "Bookmarks" tab body: list + speed-search +
/// note/mnemonic edit + delete (confirm).
export default function BookmarksPanel({ repo, onJump }: BookmarksPanelProps) {
  const q = useBookmarks(repo);
  const patch = usePatchBookmark(repo);
  const del = useDeleteBookmark(repo);
  const confirm = useConfirm();
  const [filter, setFilter] = useState("");
  const [editingNoteId, setEditingNoteId] = useState<number | null>(null);
  const [noteDraft, setNoteDraft] = useState("");
  const [editingMnemonicId, setEditingMnemonicId] = useState<number | null>(null);
  const [mnemonicDraft, setMnemonicDraft] = useState("");

  const bookmarks = q.data?.bookmarks ?? [];

  const hits = useMemo(
    () =>
      speedFilterItems(
        bookmarks,
        filter,
        (b) =>
          `${b.mnemonic ?? ""} ${b.path}:${b.line} ${b.note ?? ""}`,
      ),
    [bookmarks, filter],
  );

  async function saveNote(b: Bookmark) {
    try {
      await patch.mutateAsync({
        id: b.id,
        input: { note: noteDraft.trim() === "" ? null : noteDraft.trim() },
      });
      setEditingNoteId(null);
    } catch (e) {
      toast.err(`couldn't update note: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function saveMnemonic(b: Bookmark) {
    const raw = mnemonicDraft.trim();
    if (raw !== "" && !MNEMONIC_RE.test(raw)) {
      toast.err("mnemonic must be a single [0-9a-z] character");
      return;
    }
    try {
      await patch.mutateAsync({
        id: b.id,
        input: { mnemonic: raw === "" ? null : raw },
      });
      setEditingMnemonicId(null);
    } catch (e) {
      toast.err(`couldn't set mnemonic: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function remove(b: Bookmark) {
    const ok = await confirm({
      title: "Delete bookmark?",
      body: `${b.path}:${b.line}${b.note ? ` — ${b.note}` : ""}`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await del.mutateAsync(b.id);
    } catch (e) {
      toast.err(`couldn't delete bookmark: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  if (q.isLoading) {
    return <div className="kbc-inspector__hint">Loading bookmarks…</div>;
  }
  if (q.error) {
    return (
      <div className="kbc-inspector__hint kbc-reader__hint--error">
        {(q.error as Error).message}
      </div>
    );
  }

  return (
    <div className="kbc-bookmarks" data-kbc-bookmarks>
      <input
        className="kbc-outline__filter"
        type="search"
        value={filter}
        placeholder="Filter bookmarks…"
        aria-label="Filter bookmarks"
        onChange={(e) => setFilter(e.target.value)}
        data-kbc-bookmarks-filter
      />
      {bookmarks.length === 0 && (
        <div className="kbc-inspector__hint">No bookmarks yet. Press <kbd>gm</kbd> on a line.</div>
      )}
      {bookmarks.length > 0 && hits.length === 0 && (
        <div className="kbc-inspector__hint">No matches.</div>
      )}
      <ul className="kbc-bookmarks__list">
        {hits.map(({ item: b, ranges }) => {
          const label = `${b.path}:${b.line}`;
          return (
            <li key={b.id} className="kbc-bookmarks__row" data-kbc-bookmark-row data-kbc-bookmark-id={b.id}>
              <button
                type="button"
                className="kbc-bookmarks__jump"
                onClick={() => onJump({ path: b.path, line: b.line })}
                title={label}
              >
                <span className="kbc-bookmarks__mnemonic" data-kbc-bookmark-mnemonic>
                  {b.mnemonic ?? "·"}
                </span>
                <span className="kbc-bookmarks__path">
                  {ranges.length === 0
                    ? label
                    : highlightSegments(label, ranges.filter((r) => r.start < label.length)).map(
                        (seg, si) =>
                          seg.hit ? (
                            <mark key={si} className="kbc-speedsearch__mark">
                              {seg.text}
                            </mark>
                          ) : (
                            <span key={si}>{seg.text}</span>
                          ),
                      )}
                </span>
                {b.note && editingNoteId !== b.id && (
                  <span className="kbc-bookmarks__note">{b.note}</span>
                )}
              </button>
              <div className="kbc-bookmarks__actions">
                {editingNoteId === b.id ? (
                  <form
                    className="kbc-bookmarks__edit"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void saveNote(b);
                    }}
                  >
                    <input
                      value={noteDraft}
                      onChange={(e) => setNoteDraft(e.target.value)}
                      aria-label="bookmark note"
                      data-kbc-bookmark-note-input
                      autoFocus
                    />
                    <button type="submit">Save</button>
                    <button type="button" onClick={() => setEditingNoteId(null)}>
                      Cancel
                    </button>
                  </form>
                ) : (
                  <button
                    type="button"
                    className="kbc-bookmarks__act"
                    onClick={() => {
                      setEditingNoteId(b.id);
                      setNoteDraft(b.note ?? "");
                    }}
                    data-kbc-bookmark-edit-note
                  >
                    note
                  </button>
                )}
                {editingMnemonicId === b.id ? (
                  <form
                    className="kbc-bookmarks__edit"
                    onSubmit={(e) => {
                      e.preventDefault();
                      void saveMnemonic(b);
                    }}
                  >
                    <input
                      value={mnemonicDraft}
                      onChange={(e) => setMnemonicDraft(e.target.value.slice(0, 1))}
                      maxLength={1}
                      aria-label="bookmark mnemonic"
                      data-kbc-bookmark-mnemonic-input
                      placeholder="0-9a-z"
                      autoFocus
                    />
                    <button type="submit">Set</button>
                    <button type="button" onClick={() => setEditingMnemonicId(null)}>
                      Cancel
                    </button>
                  </form>
                ) : (
                  <button
                    type="button"
                    className="kbc-bookmarks__act"
                    onClick={() => {
                      setEditingMnemonicId(b.id);
                      setMnemonicDraft(b.mnemonic ?? "");
                    }}
                    data-kbc-bookmark-edit-mnemonic
                  >
                    mark
                  </button>
                )}
                <button
                  type="button"
                  className="kbc-bookmarks__act kbc-bookmarks__act--danger"
                  onClick={() => void remove(b)}
                  data-kbc-bookmark-delete
                >
                  del
                </button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
