import { useEffect, useMemo, useRef, useState } from "react";
import { renameFolder } from "../api/client";
import {
  isUnchangedTarget,
  validateFolderRenameTarget,
} from "../lib/movePath";
import { toast } from "../lib/toast";

// F4 — rename the gallery's active folder filter. Same native <dialog>
// shell as MoveArtifactModal / MemoryPromoteModal (focus trap + Escape).

export default function RenameFolderModal({
  kb,
  from,
  onClose,
  onRenamed,
}: {
  kb: string;
  from: string;
  onClose: () => void;
  /** New folder path after a successful rename. */
  onRenamed: (to: string, movedCount: number) => void;
}) {
  const dlgRef = useRef<HTMLDialogElement | null>(null);
  const [to, setTo] = useState(from);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const trigger = document.activeElement as HTMLElement | null;
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
    return () => trigger?.focus?.();
  }, []);

  const target = useMemo(() => validateFolderRenameTarget(to), [to]);
  const unchanged = target.ok && isUnchangedTarget(from, target.path);
  const canRename = target.ok && !unchanged && !busy;

  const submit = async () => {
    if (!target.ok || unchanged) return;
    setBusy(true);
    try {
      const r = await renameFolder(kb, from, target.path);
      const n = r.moved.length;
      toast.ok(
        n === 1
          ? `renamed folder · 1 artifact moved`
          : `renamed folder · ${n} artifacts moved`,
      );
      onRenamed(target.path, n);
      onClose();
    } catch (e) {
      toast.err(
        `rename folder failed: ${e instanceof Error ? e.message : String(e)}`,
      );
      setBusy(false);
    }
  };

  return (
    <dialog
      ref={dlgRef}
      className="kb-move"
      aria-labelledby="kb-rename-folder-title"
      onCancel={(e) => {
        e.preventDefault();
        if (!busy) onClose();
      }}
    >
      <header className="kb-move__head">
        <h2 id="kb-rename-folder-title">Rename folder</h2>
      </header>
      <div className="kb-move__body">
        <p className="kb-move__lead">
          Move every artifact under <code>{from}</code> to a new folder
          path. Nested paths stay nested under the new name.
        </p>
        <label className="kb-move__field">
          <span>New folder path</span>
          <input
            type="text"
            className="kb-move__input"
            value={to}
            onChange={(e) => setTo(e.target.value)}
            disabled={busy}
            autoFocus
            autoComplete="off"
            spellCheck={false}
            aria-label="new folder path"
          />
        </label>
        <div className="kb-move__preview" aria-live="polite">
          {target.ok ? (
            <>
              <code className="kb-move__path">{from}</code>
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
          disabled={!canRename}
        >
          {busy ? "Renaming…" : "Rename"}
        </button>
      </footer>
    </dialog>
  );
}
