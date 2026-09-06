import { useEffect, useState, type FormEvent } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { ApiError, type SetSpanInput } from "../api/client";
import type { SetSpanOut } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useConfirm } from "../components/ConfirmProvider";
import { useDocLens } from "../hooks/useDocLens";
import { useAppendSetSpan, useDeleteSet, useFromDocSet, usePatchSet, useSet } from "../hooks/useSets";
import { codeUrl } from "../lib/codeUrl";
import { shortSha } from "../lib/format";
import { setsUrl, setUrl, tourUrl } from "../lib/setsUrl";
import { toast } from "../lib/toast";
import "../styles/sets.css";

/// Repo-relative "path:start-end" label for one span — used both for the
/// row's own link text and (via `spanLabel` in `Tour.tsx`) the tour dots'
/// title text, kept in this one small helper so the two never drift.
export function spanLineLabel(s: Pick<SetSpanOut, "line_start" | "line_end">): string {
  if (s.line_start == null) return "";
  return s.line_end !== undefined && s.line_end !== s.line_start
    ? `:${s.line_start}-${s.line_end}`
    : `:${s.line_start}`;
}

function toSpanInput(s: SetSpanOut): SetSpanInput {
  return { path: s.path, line_start: s.line_start, line_end: s.line_end, ref: s.ref, note: s.note };
}

/// Phase E4 — `/r/{repo}/~sets/{id}`: one set's detail — ordered span rows
/// (path:lines, a ref chip when pinned, a note preview), reorder via
/// up/down buttons (each a `PATCH .../spans` FULL replacement —
/// `reading_sets::patch_set`'s doc), row removal, an "Add span" form
/// (path + optional start/end + note), inline rename, delete via
/// `useConfirm`, and the "▶ Tour" entry point.
export default function SetDetail() {
  const { repo = "", id = "" } = useParams<{ repo: string; id: string }>();
  const navigate = useNavigate();
  const confirm = useConfirm();
  const set = useSet(repo, id);
  const patchSet = usePatchSet(repo);
  const appendSpan = useAppendSetSpan(repo);
  const deleteSet = useDeleteSet(repo);
  const fromDoc = useFromDocSet(repo);

  // DCB W3.C — "doc changed since" provenance. `useDocLens` is a HOOK, so it
  // must be called unconditionally on every render, BEFORE the loading/
  // error/null guards below (16-w3-reverse-index.md §3.3's own sketch calls
  // it AFTER those guards, which the Rules of Hooks forbid — this is a
  // corrected placement, not a spec deviation of substance: `provenance`
  // reads `set.data` optionally, so it — and `useDocLens`'s own `enabled`
  // gate — degrade to "nothing yet" for exactly the same render(s) the
  // guards below would otherwise have skipped). `set.data.source_doc_hash`
  // may itself be `null` (a from-doc set materialized before the doc had a
  // hash, or a set from any other creation path) — carried through as-is;
  // the `changed` derivation below (computed AFTER the guards, since it's a
  // plain value, not a hook) is what applies R14's null-is-unknown rule.
  const provenance =
    set.data?.source_kb && set.data?.source_doc_id
      ? { kb: set.data.source_kb, doc: set.data.source_doc_id, hash: set.data.source_doc_hash }
      : null;
  const liveLens = useDocLens(provenance?.kb, provenance?.doc, repo);

  const [renaming, setRenaming] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const [descDraft, setDescDraft] = useState("");

  const [newPath, setNewPath] = useState("");
  const [newStart, setNewStart] = useState("");
  const [newEnd, setNewEnd] = useState("");
  const [newNote, setNewNote] = useState("");

  // Seed the rename draft once per set (not on every keystroke elsewhere in
  // the app re-rendering this route) — re-seeds if the operator navigates to
  // a DIFFERENT set entirely.
  useEffect(() => {
    if (set.data) {
      setNameDraft(set.data.name);
      setDescDraft(set.data.description ?? "");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [set.data?.id]);

  if (set.isLoading) {
    return <div className="kbc-reader__hint">Loading set…</div>;
  }
  if (set.error) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">{(set.error as Error).message}</div>;
  }
  if (!set.data) return null;

  // (reconciled: M18/R14) — `null` on EITHER side means "unknown", never
  // "changed": comparing `null !== null` would already be `false` (no
  // banner, correct), but `null !== "<a real hash>"` (or the reverse) WOULD
  // be `true` under a naive `!==` — a false-positive banner on a doc whose
  // live hash simply isn't known yet (excluded by `[doclens] kbs`, or never
  // reindexed since DCB). Both sides must be non-null AND unequal.
  const changed =
    provenance != null &&
    liveLens.data != null &&
    provenance.hash != null &&
    liveLens.data.doc_hash != null &&
    liveLens.data.doc_hash !== provenance.hash;

  async function rematerialize() {
    if (!provenance) return;
    try {
      const view = await fromDoc.mutateAsync({ kb: provenance.kb, doc: provenance.doc });
      navigate(setUrl(repo, view.id));
    } catch (e) {
      if (e instanceof ApiError && e.status === 403) {
        toast.err(
          "Re-materializing a reading set is LOOPBACK-ONLY — it only works when kb-code is reached at 127.0.0.1.",
        );
      } else {
        toast.err(`couldn't re-materialize: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  }

  const spans = set.data.spans;

  async function reorder(index: number, dir: -1 | 1) {
    const target = index + dir;
    if (target < 0 || target >= spans.length) return;
    const next = [...spans];
    [next[index], next[target]] = [next[target], next[index]];
    try {
      await patchSet.mutateAsync({ id, input: { spans: next.map(toSpanInput) } });
    } catch (e) {
      toast.err(`couldn't reorder: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function removeRow(index: number) {
    const next = spans.filter((_, i) => i !== index);
    try {
      await patchSet.mutateAsync({ id, input: { spans: next.map(toSpanInput) } });
    } catch (e) {
      toast.err(`couldn't remove span: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function saveRename() {
    const n = nameDraft.trim();
    if (!n) return;
    try {
      await patchSet.mutateAsync({ id, input: { name: n, description: descDraft.trim() || undefined } });
      setRenaming(false);
    } catch (e) {
      toast.err(`couldn't rename set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function addSpan(e: FormEvent) {
    e.preventDefault();
    const path = newPath.trim();
    if (!path) return;
    const start = newStart.trim() ? parseInt(newStart, 10) : undefined;
    const end = newEnd.trim() ? parseInt(newEnd, 10) : undefined;
    try {
      await appendSpan.mutateAsync({
        id,
        input: { path, line_start: start, line_end: end, note: newNote.trim() || undefined },
      });
      setNewPath("");
      setNewStart("");
      setNewEnd("");
      setNewNote("");
    } catch (e2) {
      toast.err(`couldn't add span: ${e2 instanceof Error ? e2.message : String(e2)}`);
    }
  }

  async function removeSet() {
    const ok = await confirm({
      title: `Delete "${set.data!.name}"?`,
      body: "This can't be undone.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteSet.mutateAsync(id);
      navigate(setsUrl(repo));
    } catch (e) {
      toast.err(`couldn't delete set: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return (
    <div className="kbc-sets" id="main">
      <header className="kbc-sets__detail-head">
        <div className="kbc-sets__detail-head-main">
          {renaming ? (
            <form
              className="kbc-sets__rename-form"
              onSubmit={(e) => {
                e.preventDefault();
                void saveRename();
              }}
            >
              <input
                type="text"
                value={nameDraft}
                onChange={(e) => setNameDraft(e.target.value)}
                aria-label="set name"
                data-kbc-set-name-input
              />
              <input
                type="text"
                value={descDraft}
                onChange={(e) => setDescDraft(e.target.value)}
                aria-label="set description"
                placeholder="Description"
                data-kbc-set-desc-input
              />
              <button type="submit" data-kbc-set-rename-save>
                Save
              </button>
              <button type="button" onClick={() => setRenaming(false)}>
                Cancel
              </button>
            </form>
          ) : (
            <>
              <div className="kbc-sets__title-row">
                <h1 className="kbc-sets__title">{set.data.name}</h1>
                <button
                  type="button"
                  className="kbc-sets__rename-trigger"
                  onClick={() => setRenaming(true)}
                  title="Rename this set"
                  data-kbc-set-title
                >
                  Rename
                </button>
              </div>
              {set.data.description && <p className="kbc-sets__desc">{set.data.description}</p>}
            </>
          )}
        </div>
        <div className="kbc-sets__detail-actions">
          {spans.length > 0 && (
            <Link to={tourUrl(repo, id)} className="kbc-sets__tour-btn" data-kbc-set-tour>
              ▶ Tour
            </Link>
          )}
          <button
            type="button"
            className="kbc-sets__delete"
            onClick={() => void removeSet()}
            data-kbc-set-delete
          >
            Delete set
          </button>
        </div>
      </header>

      {changed && (
        <div className="kbc-sets__stale-banner" data-kbc-set-stale-banner>
          This set's source doc has changed since it was materialized.
          <button
            type="button"
            onClick={() => void rematerialize()}
            disabled={fromDoc.isPending}
            data-kbc-set-rematerialize
          >
            {fromDoc.isPending ? "Re-materializing…" : "Re-materialize as new set"}
          </button>
        </div>
      )}

      {spans.length === 0 ? (
        <EmptyState
          icon={<Icon.List />}
          title="No spans yet"
          hint="Add a file/range below to start this reading set."
        />
      ) : (
        <ol className="kbc-sets__spans" data-kbc-set-spans>
          {spans.map((s, i) => (
            <li key={s.ordinal} className="kbc-sets__span-row" data-kbc-set-span={s.ordinal}>
              <Link
                to={codeUrl({
                  repo,
                  path: s.path,
                  ref: s.ref,
                  line: s.line_start != null && s.line_end != null ? { start: s.line_start, end: s.line_end } : undefined,
                })}
                className="kbc-sets__span-link"
                data-kbc-set-span-link={s.ordinal}
              >
                {s.path}
                {spanLineLabel(s)}
              </Link>
              {s.ref && (
                <span
                  className="kbc-sets__span-ref"
                  data-kbc-set-span-ref={s.ordinal}
                  title={s.ref}
                >
                  {shortSha(s.ref)}
                </span>
              )}
              {s.note && <span className="kbc-sets__span-note">{s.note}</span>}
              <div className="kbc-sets__span-actions">
                <button
                  type="button"
                  onClick={() => void reorder(i, -1)}
                  disabled={i === 0}
                  aria-label="move up"
                  title="Move up"
                  data-kbc-set-span-up={s.ordinal}
                >
                  ▲
                </button>
                <button
                  type="button"
                  onClick={() => void reorder(i, 1)}
                  disabled={i === spans.length - 1}
                  aria-label="move down"
                  title="Move down"
                  data-kbc-set-span-down={s.ordinal}
                >
                  ▼
                </button>
                <button
                  type="button"
                  onClick={() => void removeRow(i)}
                  aria-label="remove span"
                  title="Remove"
                  data-kbc-set-span-remove={s.ordinal}
                >
                  <Icon.X />
                </button>
              </div>
            </li>
          ))}
        </ol>
      )}

      <form className="kbc-sets__add-span" onSubmit={(e) => void addSpan(e)} data-kbc-set-add-span-form>
        <h2 className="kbc-sets__add-span-title">Add span</h2>
        <div className="kbc-sets__add-span-row">
          <input
            type="text"
            placeholder="path/to/file.rs"
            value={newPath}
            onChange={(e) => setNewPath(e.target.value)}
            aria-label="span path"
            data-kbc-set-add-path
          />
          <input
            type="number"
            min={1}
            placeholder="start"
            value={newStart}
            onChange={(e) => setNewStart(e.target.value)}
            aria-label="start line"
            data-kbc-set-add-start
          />
          <input
            type="number"
            min={1}
            placeholder="end"
            value={newEnd}
            onChange={(e) => setNewEnd(e.target.value)}
            aria-label="end line"
            data-kbc-set-add-end
          />
        </div>
        <input
          type="text"
          placeholder="Note (optional)"
          value={newNote}
          onChange={(e) => setNewNote(e.target.value)}
          aria-label="span note"
          data-kbc-set-add-note
        />
        <button type="submit" disabled={!newPath.trim() || appendSpan.isPending} data-kbc-set-add-submit>
          {appendSpan.isPending ? "Adding…" : "Add span"}
        </button>
      </form>
    </div>
  );
}
