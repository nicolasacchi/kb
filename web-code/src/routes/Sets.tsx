import { useState, type FormEvent } from "react";
import { Link, useParams } from "react-router";
import type { SetSummary } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useConfirm } from "../components/ConfirmProvider";
import { useCreateSet, useDeleteSet, usePatchSet, useSets } from "../hooks/useSets";
import { relativeTime } from "../lib/format";
import { setUrl } from "../lib/setsUrl";
import { toast } from "../lib/toast";
import "../styles/sets.css";
import { useListScrollRestoration } from "../hooks/useScrollRestoration";

/// Phase E4 ("kb-code v2 — The Operable Reader") — `/r/{repo}/~sets`: the
/// reading-sets list. One row per set (name, description, span count,
/// relative update time), a create form, inline rename, and delete via
/// `useConfirm` (never `window.confirm` — root CLAUDE.md invariant #32).
/// Kept fresh by `api/queryClient.ts`'s `set.changed` SSE handler, not
/// polling.
export default function Sets() {
  // V70-A6 — root CLAUDE.md #31, ported: this list scrolls the WINDOW, so
  // one offset keyed on the full URL is the whole story. Back onto it lands
  // where the reader left, not where the browser guessed.
  useListScrollRestoration();
  const { repo = "" } = useParams<{ repo: string }>();
  const sets = useSets(repo);
  const createSet = useCreateSet(repo);
  const deleteSet = useDeleteSet(repo);
  const patchSet = usePatchSet(repo);
  const confirm = useConfirm();

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  async function create(e: FormEvent) {
    e.preventDefault();
    const n = name.trim();
    if (!n) return;
    try {
      await createSet.mutateAsync({ repo, name: n, description: description.trim() || undefined });
      setName("");
      setDescription("");
    } catch (e2) {
      toast.err(`couldn't create set: ${e2 instanceof Error ? e2.message : String(e2)}`);
    }
  }

  async function remove(s: SetSummary) {
    const ok = await confirm({
      title: `Delete "${s.name}"?`,
      body: `This removes all ${s.span_count} span${s.span_count === 1 ? "" : "s"} — this can't be undone.`,
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteSet.mutateAsync(s.id);
    } catch (e) {
      toast.err(`couldn't delete set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  function startRename(s: SetSummary) {
    setRenamingId(s.id);
    setRenameDraft(s.name);
  }

  async function saveRename(id: string) {
    const n = renameDraft.trim();
    if (!n) return;
    try {
      await patchSet.mutateAsync({ id, input: { name: n } });
      setRenamingId(null);
    } catch (e) {
      toast.err(`couldn't rename set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  const list = sets.data?.sets ?? [];

  return (
    <div className="kbc-sets" id="main">
      <header className="kbc-sets__head">
        <h1 className="kbc-sets__title">Reading sets — {repo}</h1>
        <p className="kbc-sets__hint">
          Ordered file/span walkthroughs — build one from the reader's "+ Set" menu, or start here.
        </p>
      </header>

      <form className="kbc-sets__create" onSubmit={(e) => void create(e)} data-kbc-sets-create-form>
        <input
          type="text"
          placeholder="Name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          aria-label="new set name"
          data-kbc-sets-create-name
        />
        <input
          type="text"
          placeholder="Description (optional)"
          value={description}
          onChange={(e) => setDescription(e.target.value)}
          aria-label="new set description"
          data-kbc-sets-create-desc
        />
        <button type="submit" disabled={!name.trim() || createSet.isPending} data-kbc-sets-create-submit>
          {createSet.isPending ? "Creating…" : "Create set"}
        </button>
      </form>

      {sets.isLoading ? (
        <div className="kbc-reader__hint">Loading sets…</div>
      ) : sets.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(sets.error as Error).message}</div>
      ) : list.length === 0 ? (
        <EmptyState
          icon={<Icon.List />}
          title="No reading sets yet"
          hint="Create one above, or add a file to a new set from the reader's + Set menu."
        />
      ) : (
        <ul className="kbc-sets__list" data-kbc-sets-list>
          {list.map((s) => (
            <li key={s.id} className="kbc-sets__row" data-kbc-sets-row={s.id}>
              {renamingId === s.id ? (
                <form
                  className="kbc-sets__rename-form"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void saveRename(s.id);
                  }}
                >
                  <input
                    type="text"
                    value={renameDraft}
                    onChange={(e) => setRenameDraft(e.target.value)}
                    aria-label="rename set"
                    autoFocus
                    data-kbc-sets-rename-input
                  />
                  <button type="submit" data-kbc-sets-rename-save>
                    Save
                  </button>
                  <button type="button" onClick={() => setRenamingId(null)}>
                    Cancel
                  </button>
                </form>
              ) : (
                <>
                  <Link to={setUrl(repo, s.id)} className="kbc-sets__row-name" data-kbc-sets-row-link={s.id}>
                    {s.name}
                  </Link>
                  {s.description && <span className="kbc-sets__row-desc">{s.description}</span>}
                  <span className="kbc-sets__row-meta">
                    {s.span_count} span{s.span_count === 1 ? "" : "s"} · updated {relativeTime(s.updated_at)}
                  </span>
                  <div className="kbc-sets__row-actions">
                    <button type="button" onClick={() => startRename(s)} data-kbc-sets-row-rename={s.id}>
                      Rename
                    </button>
                    <button type="button" onClick={() => void remove(s)} data-kbc-sets-row-delete={s.id}>
                      Delete
                    </button>
                  </div>
                </>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
