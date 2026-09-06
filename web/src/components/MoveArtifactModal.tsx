import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchFolders, moveDoc } from "../api/client";
import { childFolders } from "../lib/folderTree";
import {
  isUnchangedTarget,
  joinMoveTarget,
} from "../lib/movePath";
import { toast } from "../lib/toast";

// F4 — move (or rename-in-place) the open artifact. Native <dialog>
// via .showModal() for focus trap + ::backdrop + Escape, same shell as
// MemoryPromoteModal / ConfirmModal (invariant #32 — never window.confirm).

export default function MoveArtifactModal({
  kb,
  artifactId,
  sourceRel,
  onClose,
  onMoved,
}: {
  kb: string;
  artifactId: string;
  sourceRel: string;
  onClose: () => void;
  /** Called with the server's new source-relative path after a successful move. */
  onMoved: (newSourceRel: string) => void;
}) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  const slash = sourceRel.lastIndexOf("/");
  const initialFolder = slash >= 0 ? sourceRel.slice(0, slash) : "";
  const initialFilename =
    slash >= 0 ? sourceRel.slice(slash + 1) : sourceRel;

  const [cwd, setCwd] = useState(initialFolder);
  const [newSubfolder, setNewSubfolder] = useState("");
  const [filename, setFilename] = useState(initialFilename);
  const [busy, setBusy] = useState(false);

  const foldersQuery = useQuery({
    queryKey: ["folders", kb],
    queryFn: ({ signal }) => fetchFolders(kb, signal),
    staleTime: Infinity,
  });

  const subfolders = useMemo(
    () => childFolders(foldersQuery.data?.folders ?? [], cwd),
    [foldersQuery.data, cwd],
  );

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
    return () => trigger?.focus?.();
  }, []);

  const target = useMemo(
    () =>
      joinMoveTarget({
        folder: cwd,
        newSubfolder,
        filename,
      }),
    [cwd, newSubfolder, filename],
  );

  const targetPath = target.ok ? target.path : null;
  const unchanged =
    targetPath != null && isUnchangedTarget(sourceRel, targetPath);
  const canMove = target.ok && !unchanged && !busy;

  const segs = cwd ? cwd.split("/").filter(Boolean) : [];
  const cumulative: string[] = [];
  segs.forEach((seg, i) => {
    cumulative.push(i === 0 ? seg : `${cumulative[i - 1]}/${seg}`);
  });

  const submit = async () => {
    if (!target.ok || unchanged) return;
    setBusy(true);
    try {
      const r = await moveDoc(kb, artifactId, target.path);
      toast.ok(`moved to ${r.new_source_rel}`);
      onMoved(r.new_source_rel);
      onClose();
    } catch (e) {
      toast.err(
        `move failed: ${e instanceof Error ? e.message : String(e)}`,
      );
      setBusy(false);
    }
  };

  return (
    <dialog
      ref={dlgRef}
      className="kb-move"
      aria-labelledby="kb-move-title"
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
    >
      <header className="kb-move__head">
        <h2 id="kb-move-title">Move artifact</h2>
      </header>
      <div className="kb-move__body">
        <p className="kb-move__lead">
          Choose a destination folder, optionally a new subfolder, and the
          filename. Same folder + new name is a rename.
        </p>

        <div className="kb-move__field">
          <span>Destination folder</span>
          <nav className="kb-move__crumbs" aria-label="destination folder">
            <button
              type="button"
              className={`kb-move__crumb${cwd === "" ? " is-here" : ""}`}
              onClick={() => setCwd("")}
              disabled={busy}
            >
              (root)
            </button>
            {segs.map((seg, i) => (
              <span key={cumulative[i]} className="kb-move__crumb-wrap">
                <span className="kb-move__crumb-sep" aria-hidden>
                  /
                </span>
                <button
                  type="button"
                  className={`kb-move__crumb${cwd === cumulative[i] ? " is-here" : ""}`}
                  onClick={() => setCwd(cumulative[i])}
                  disabled={busy}
                >
                  {seg}
                </button>
              </span>
            ))}
          </nav>
          <div className="kb-move__dirs" role="list">
            {subfolders.length === 0 && (
              <div className="kb-move__dirs-empty">
                {foldersQuery.isLoading ? "loading…" : "no subfolders"}
              </div>
            )}
            {subfolders.map((sf) => (
              <button
                key={sf.path}
                type="button"
                role="listitem"
                className="kb-move__dir"
                onClick={() => setCwd(sf.path)}
                disabled={busy}
                title={`${sf.path} (${sf.count})`}
              >
                <span className="kb-move__dir-name">▸ {sf.name}</span>
                <span className="kb-move__dir-count">{sf.count}</span>
              </button>
            ))}
          </div>
        </div>

        <label className="kb-move__field">
          <span>New subfolder (optional)</span>
          <input
            type="text"
            className="kb-move__input"
            value={newSubfolder}
            onChange={(e) => setNewSubfolder(e.target.value)}
            placeholder="e.g. archive"
            disabled={busy}
            autoComplete="off"
            spellCheck={false}
          />
        </label>

        <label className="kb-move__field">
          <span>Filename</span>
          <input
            type="text"
            className="kb-move__input"
            value={filename}
            onChange={(e) => setFilename(e.target.value)}
            disabled={busy}
            autoComplete="off"
            spellCheck={false}
            aria-label="filename"
          />
        </label>

        <div className="kb-move__preview" aria-live="polite">
          {target.ok ? (
            <>
              <code className="kb-move__path">{sourceRel}</code>
              <span className="kb-move__arrow" aria-hidden>
                {" → "}
              </span>
              <code className="kb-move__path">{target.path}</code>
              {unchanged && (
                <span className="kb-move__hint"> (unchanged)</span>
              )}
            </>
          ) : (
            <span className="kb-move__preview-err" role="alert">
              {target.error}
            </span>
          )}
        </div>
      </div>
      <footer className="kb-move__foot">
        <button
          type="button"
          className="kb-move__btn"
          onClick={onClose}
          disabled={busy}
        >
          Cancel
        </button>
        <button
          type="button"
          className="kb-move__btn kb-move__btn--primary"
          onClick={submit}
          disabled={!canMove}
        >
          {busy ? "Moving…" : "Move"}
        </button>
      </footer>
    </dialog>
  );
}
