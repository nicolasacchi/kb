import { useState } from "react";
import type { SetSpanInput } from "../../api/client";
import { useAppendSetSpan, useCreateSet, useSets } from "../../hooks/useSets";
import { setUrl } from "../../lib/setsUrl";
import { toast } from "../../lib/toast";

export interface AddToSetMenuProps {
  repo: string;
  /// The FOCUSED pane's currently-open file — this menu only ever mounts
  /// while one is open (`Reader.tsx`'s own gate).
  path: string;
  /// Returns the FOCUSED pane's current selection lines (`start === end`
  /// for a plain cursor, never a drag-selected range) — read ONCE at click
  /// time, mirroring `Reader.tsx`'s own `capturePeekAnchor` idiom for
  /// grabbing a live CM6 fact off a ref rather than re-rendering on every
  /// keystroke for a menu that isn't even open most of the time.
  getSelection: () => { start: number; end: number } | null;
}

function currentSpanInput(path: string, getSelection: () => { start: number; end: number } | null): SetSpanInput {
  const sel = getSelection();
  if (sel && sel.start !== sel.end) {
    return { path, line_start: sel.start, line_end: sel.end };
  }
  return { path };
}

/// Phase E4 — the reader's "+ Set" capture affordance: a small menu (mirrors
/// `RefPicker.tsx`'s own trigger-button + `<ul role="listbox">` shape) that
/// either appends the open file (or its current selection, when one exists)
/// to an EXISTING reading set, or creates a brand-new one carrying that
/// first span. Never navigates on its own — a toast (with a link to the
/// set) confirms the capture, so the operator stays put in the reader.
export default function AddToSetMenu({ repo, path, getSelection }: AddToSetMenuProps) {
  const [open, setOpen] = useState(false);
  const [newName, setNewName] = useState("");
  const sets = useSets(repo);
  const createSet = useCreateSet(repo);
  const appendSpan = useAppendSetSpan(repo);

  async function addTo(setId: string, setName: string) {
    setOpen(false);
    try {
      await appendSpan.mutateAsync({ id: setId, input: currentSpanInput(path, getSelection) });
      toast.ok(`Added to "${setName}"`, { to: setUrl(repo, setId), label: "View set" });
    } catch (e) {
      toast.err(`couldn't add to set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function createAndAdd() {
    const name = newName.trim();
    if (!name) return;
    setOpen(false);
    try {
      const created = await createSet.mutateAsync({
        repo,
        name,
        spans: [currentSpanInput(path, getSelection)],
      });
      setNewName("");
      toast.ok(`Created "${created.name}"`, { to: setUrl(repo, created.id), label: "View set" });
    } catch (e) {
      toast.err(`couldn't create set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  const list = sets.data?.sets ?? [];

  return (
    <div className="kbc-addset">
      <button
        type="button"
        className="kbc-addset__trigger"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        title="Add this file (or the current selection) to a reading set"
        data-kbc-addset-trigger
      >
        + Set
      </button>
      {open && (
        <div className="kbc-addset__menu" role="menu" data-kbc-addset-menu>
          {sets.isLoading ? (
            <div className="kbc-addset__hint">Loading sets…</div>
          ) : list.length === 0 ? (
            <div className="kbc-addset__hint">No sets yet.</div>
          ) : (
            <ul className="kbc-addset__list">
              {list.map((s) => (
                <li key={s.id}>
                  <button
                    type="button"
                    className="kbc-addset__item"
                    onClick={() => void addTo(s.id, s.name)}
                    data-kbc-addset-item={s.id}
                  >
                    {s.name}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <form
            className="kbc-addset__new"
            onSubmit={(e) => {
              e.preventDefault();
              void createAndAdd();
            }}
          >
            <input
              type="text"
              className="kbc-addset__new-input"
              placeholder="New set name"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              aria-label="new set name"
              data-kbc-addset-new-input
            />
            <button
              type="submit"
              className="kbc-addset__create"
              disabled={!newName.trim()}
              data-kbc-addset-create
            >
              Create
            </button>
          </form>
        </div>
      )}
    </div>
  );
}
