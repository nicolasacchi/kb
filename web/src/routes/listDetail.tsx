import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import {
  exportListShareBundle,
  listExportUrl,
  lookupArtifact,
  pruneList,
  type ListEntry,
  type LookupHit,
} from "../api/client";
import { useListDetail } from "../hooks/useLists";
import { useDocumentTitle } from "../hooks/useDocumentTitle";
import { continueTarget, entryTrailHref, nextReadToggle } from "../lib/listTrail";
import { ListProgressBar, listStatsLine } from "../components/lists/ListCard";
import EntryRow from "../components/lists/EntryRow";
import { useConfirm } from "../components/ConfirmProvider";
import { saveBlob } from "../lib/download";
import { toast } from "../lib/toast";

// RL-track (v0.18) — /lists/:kb/:id: one reading list. Ordered entries
// with derived read state, click-to-edit header + notes, ↑/↓ + drag
// reorder, add-by-path (resolved via /lookup with an inline ambiguity
// picker), md/json export links. All server state rides the
// ["list", kb, id] cache entry; SSE reconciles every mutation.

function AddEntryRow({
  kb,
  onAdd,
}: {
  kb: string;
  onAdd: (artifactId: string) => Promise<void>;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [candidates, setCandidates] = useState<LookupHit[]>([]);
  const [error, setError] = useState<string | null>(null);

  const add = async (artifactId: string) => {
    try {
      await onAdd(artifactId);
      setValue("");
      setCandidates([]);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const submit = async () => {
    const q = value.trim();
    if (!q || busy) return;
    setBusy(true);
    setError(null);
    setCandidates([]);
    try {
      // A bare 12-hex token IS an artifact id — skip the lookup.
      if (/^[0-9a-f]{12}$/.test(q)) {
        await add(q);
        return;
      }
      const r = await lookupArtifact(kb, q);
      if (r.kind === "exact" || r.kind === "unique_suffix") {
        await add(r.id);
      } else if (r.kind === "ambiguous") {
        setCandidates(r.candidates);
      } else {
        setError(`no artifact matches ${JSON.stringify(q)} in ${kb}`);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="kb-listd__add">
      <div className="kb-listd__add-row">
        <input
          type="text"
          className="kb-listd__add-input"
          placeholder="Add an artifact — path, filename, or 12-hex id…"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void submit();
          }}
          aria-label="add entry"
        />
        <button
          type="button"
          className="kb-listd__add-btn"
          onClick={() => void submit()}
          disabled={!value.trim() || busy}
        >
          Add
        </button>
      </div>
      {error && <div className="kb-listd__add-err">{error}</div>}
      {candidates.length > 0 && (
        <div className="kb-listd__add-picker" role="listbox">
          {candidates.map((c) => (
            <button
              key={c.id}
              type="button"
              className="kb-listd__add-cand"
              onClick={() => void add(c.id)}
            >
              <span className="kb-listd__add-cand-title">
                {c.title ?? c.source_relative}
              </span>
              <span className="kb-listd__add-cand-path">
                {c.source_relative}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function ListDetailRoute() {
  const params = useParams<{ kb: string; id: string }>();
  const { kb, id } = params;
  const navigate = useNavigate();
  const confirm = useConfirm();
  const {
    list,
    entries,
    loading,
    error,
    addEntry,
    updateNote,
    setReadOverride,
    moveBy,
    moveBefore,
    removeEntry,
    patchHeader,
    removeList,
  } = useListDetail(kb, id);
  const [editingTitle, setEditingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const [editingDesc, setEditingDesc] = useState(false);
  const [descDraft, setDescDraft] = useState("");
  const [, setDragging] = useState<string | null>(null);
  const [shareBusy, setShareBusy] = useState(false);
  useDocumentTitle(list?.title ?? null);

  if (!kb || !id) return <div className="empty">missing list address</div>;
  if (error) {
    return (
      <div className="kb-listd__error" role="alert">
        {error} — <Link to="/lists">back to lists</Link>
      </div>
    );
  }
  if (loading || !list) return <div className="empty">loading…</div>;

  const saveTitle = () => {
    setEditingTitle(false);
    const t = titleDraft.trim();
    if (t && t !== list.title) void patchHeader({ title: t });
  };
  const saveDesc = () => {
    setEditingDesc(false);
    const d = descDraft.trim();
    if (d !== (list.description ?? ""))
      void patchHeader({ description: d === "" ? null : d });
  };

  const toggleRead = (e: ListEntry) =>
    void setReadOverride(e.id, nextReadToggle(e));

  const cont = continueTarget(entries);
  const contHref = cont && entryTrailHref(kb, id, cont);

  const onDelete = async () => {
    const ok = await confirm({
      title: "Delete this reading list?",
      body: `Delete reading list “${list.title}”? This can't be undone.`,
      confirmLabel: "Delete list",
    });
    if (!ok) return;
    void removeList().then(() => navigate("/lists"));
  };

  // v0.33 Y3 — prune tombstoned entries. SSE `list.updated` refreshes the
  // cache (#23); no manual refetch.
  const tombstoneCount = entries.filter((e) => e.tombstone).length;
  const onPrune = async () => {
    const n = tombstoneCount;
    if (n < 1) return;
    const ok = await confirm({
      title: "Clean up missing entries?",
      body: `Remove ${n} missing entr${n === 1 ? "y" : "ies"} from “${list.title}”? This can't be undone.`,
      confirmLabel: n === 1 ? "Remove 1 missing" : `Remove ${n} missing`,
    });
    if (!ok) return;
    try {
      const r = await pruneList(kb, id);
      toast.ok(
        r.removed === 1
          ? "removed 1 missing entry"
          : `removed ${r.removed} missing entries`,
      );
    } catch (e) {
      toast.err(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="kb-listd" data-testid="list-detail">
      <header className="kb-listd__head">
        <Link className="kb-listd__back" to="/lists">
          ← All lists
        </Link>
        <div className="kb-listd__titlerow">
          {editingTitle ? (
            <input
              type="text"
              className="kb-listd__title-edit"
              value={titleDraft}
              autoFocus
              onChange={(e) => setTitleDraft(e.target.value)}
              onBlur={saveTitle}
              onKeyDown={(e) => {
                if (e.key === "Enter") saveTitle();
                if (e.key === "Escape") setEditingTitle(false);
              }}
              aria-label="list title"
            />
          ) : (
            <h1
              className="kb-listd__title"
              title="click to rename"
              onClick={() => {
                setTitleDraft(list.title);
                setEditingTitle(true);
              }}
            >
              {list.title}
            </h1>
          )}
          <span className="kb-list-card__kb">{list.kb}</span>
          <span className="kb-list-card__spacer" />
          <button
            type="button"
            className={`kb-list-card__act ${list.pinned ? "is-on" : ""}`}
            onClick={() => void patchHeader({ pinned: !list.pinned })}
            aria-pressed={list.pinned}
          >
            {list.pinned ? "unpin" : "pin"}
          </button>
          <button
            type="button"
            className={`kb-list-card__act ${list.archived ? "is-on" : ""}`}
            onClick={() => void patchHeader({ archived: !list.archived })}
            aria-pressed={list.archived}
          >
            {list.archived ? "unarchive" : "archive"}
          </button>
          {tombstoneCount > 0 && (
            <button
              type="button"
              className="kb-list-card__act kb-listd__prune"
              onClick={() => void onPrune()}
              data-kb-act="list-prune"
              title={`remove ${tombstoneCount} tombstoned entr${tombstoneCount === 1 ? "y" : "ies"}`}
            >
              {tombstoneCount === 1
                ? "Clean up 1 missing"
                : `Clean up ${tombstoneCount} missing`}
            </button>
          )}
          <button
            type="button"
            className="kb-list-card__act kb-listd__delete"
            onClick={onDelete}
          >
            delete
          </button>
        </div>
        {editingDesc ? (
          <input
            type="text"
            className="kb-listd__desc-edit"
            value={descDraft}
            autoFocus
            onChange={(e) => setDescDraft(e.target.value)}
            onBlur={saveDesc}
            onKeyDown={(e) => {
              if (e.key === "Enter") saveDesc();
              if (e.key === "Escape") setEditingDesc(false);
            }}
            aria-label="list description"
          />
        ) : (
          <button
            type="button"
            className={`kb-listd__desc ${list.description ? "" : "is-empty"}`}
            onClick={() => {
              setDescDraft(list.description ?? "");
              setEditingDesc(true);
            }}
            title="edit description"
          >
            {list.description ?? "add a description…"}
          </button>
        )}
        <div className="kb-listd__progressrow">
          <ListProgressBar list={list} />
          <span className="kb-list-card__stats">{listStatsLine(list)}</span>
          {contHref && (
            <Link className="kb-list-card__continue" to={contHref}>
              Continue reading
            </Link>
          )}
          {/* W2.4 — quiet: no board is created here, the GET route's
              empty default makes it implicit on first visit. */}
          <Link className="kb-listd__board-link" to={`/board/${kb}/${id}`}>
            open as board
          </Link>
        </div>
      </header>

      <AddEntryRow
        kb={kb}
        onAdd={async (artifactId) => {
          await addEntry({ artifact_id: artifactId });
        }}
      />

      {entries.length === 0 ? (
        <div className="empty">
          No entries yet — add one above, or from the CLI:
          <code> kb list add "{list.title}" &lt;path&gt;</code>
        </div>
      ) : (
        <ol className="kb-listd__entries">
          {entries.map((e) => (
            <EntryRow
              key={e.id}
              entry={e}
              kb={kb}
              listId={id}
              onToggleRead={toggleRead}
              onNote={(eid, note) => void updateNote(eid, note)}
              onMoveBy={(eid, d) => void moveBy(eid, d)}
              onDropBefore={(dragged, target) =>
                void moveBefore(dragged, target)
              }
              onRemove={(eid) => void removeEntry(eid)}
              onDragState={setDragging}
            />
          ))}
        </ol>
      )}

      <footer className="kb-listd__foot">
        <span className="kb-listd__export-lbl">export:</span>
        <a href={listExportUrl(kb, id, "md")} download className="kb-listd__export">
          markdown
        </a>
        <a href={listExportUrl(kb, id, "json")} download className="kb-listd__export">
          json
        </a>
        <button
          type="button"
          className="kb-listd__export"
          disabled={shareBusy}
          onClick={() => {
            if (!id || shareBusy) return;
            setShareBusy(true);
            exportListShareBundle(kb, id)
              .then((r) => {
                saveBlob(r.filename, r.blob);
                if (r.skipped.length > 0) {
                  toast.info(
                    `Shared ${r.files} file(s); skipped ${r.skipped.length} tombstoned/unresolvable entr${r.skipped.length === 1 ? "y" : "ies"}`,
                  );
                } else if (r.danglers.length > 0) {
                  toast.info(
                    `Shared ${r.files} file(s); ${r.danglers.length} out-of-list link(s) left as-is`,
                  );
                } else {
                  toast.ok(`Shared ${r.files} file(s) as ${r.filename}`);
                }
              })
              .catch((e) => {
                toast.err(e instanceof Error ? e.message : String(e));
              })
              .finally(() => setShareBusy(false));
          }}
        >
          {shareBusy ? "sharing…" : "Share (.zip)"}
        </button>
      </footer>
    </div>
  );
}
