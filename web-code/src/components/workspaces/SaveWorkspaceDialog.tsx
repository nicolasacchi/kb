import { useEffect, useRef, useState, type FormEvent } from "react";
import type { DeskState } from "../../desk/deskState";
import type { SetView } from "../../api/types";
import { useCreateSet } from "../../hooks/useSets";
import {
  buildWorkspaceSnapshot,
  type WorkspaceDrawerTabSnapshot,
  type WorkspacePaneSnapshot,
} from "../../lib/workspaceSnapshot";
import { toast } from "../../lib/toast";

export interface SaveWorkspaceDialogProps {
  repo: string;
  onClose: () => void;
  onSaved: (view: SetView) => void;
  /// Prefilled from the current ref chip / HEAD branch — still editable.
  defaultRef: string | undefined;
  /// The working set, in order, each with its captured cursor line when it
  /// was one of the two open panes at save time (see
  /// `lib/workspaceSnapshot.ts`'s doc).
  files: { path: string; line: number | undefined }[];
  desk: DeskState;
  drawerTabs: WorkspaceDrawerTabSnapshot[];
  focusedPane: 1 | 2;
  /// The exact file (+ line) in each pane at save time — the PRIMARY
  /// navigation target `~workspaces`' Open action restores to (`null` pane1
  /// only when no file was open at all; `null` pane2 when no split was
  /// open).
  pane1: WorkspacePaneSnapshot | null;
  pane2: WorkspacePaneSnapshot | null;
}

/// V70-A10 ("Workspaces v0", D26) — the "Save workspace" dialog
/// (`data-cmd="desk.save-workspace"` triggers it, `Desk.tsx`): name +
/// description + a prefilled, editable ref, `POST /api/sets` with
/// `kind: "workspace"` and the desk snapshot as `desk_json`. Same
/// `<dialog>`/`useConfirm`-adjacent modal convention `StartReviewDialog`
/// uses — never `window.prompt`.
export default function SaveWorkspaceDialog({
  repo,
  onClose,
  onSaved,
  defaultRef,
  files,
  desk,
  drawerTabs,
  focusedPane,
  pane1,
  pane2,
}: SaveWorkspaceDialogProps) {
  const create = useCreateSet(repo);
  const dlgRef = useRef<HTMLDialogElement | null>(null);

  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [gitRef, setGitRef] = useState(defaultRef ?? "");

  useEffect(() => {
    const dlg = dlgRef.current;
    if (dlg && !dlg.open) dlg.showModal();
  }, []);

  useEffect(() => {
    const dlg = dlgRef.current;
    if (!dlg) return;
    const onCancel = (e: Event) => {
      e.preventDefault();
      onClose();
    };
    dlg.addEventListener("cancel", onCancel);
    return () => dlg.removeEventListener("cancel", onCancel);
  }, [onClose]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    const n = name.trim();
    if (!n) return;
    const lines: Record<string, number> = {};
    for (const f of files) {
      if (f.line !== undefined) lines[f.path] = f.line;
    }
    const snapshot = buildWorkspaceSnapshot({ desk, drawerTabs, lines, focusedPane, pane1, pane2 });
    try {
      const view = await create.mutateAsync({
        repo,
        name: n,
        kind: "workspace",
        description: description.trim() || undefined,
        ref: gitRef.trim() || undefined,
        desk_json: JSON.stringify(snapshot),
        spans: files.map((f) => ({
          path: f.path,
          line_start: f.line,
          line_end: f.line,
        })),
      });
      onSaved(view);
    } catch (e2) {
      toast.err(`couldn't save workspace: ${e2 instanceof Error ? e2.message : String(e2)}`);
    }
  }

  return (
    <dialog ref={dlgRef} className="confirm" aria-labelledby="kbc-save-workspace-title" data-kbc-save-workspace>
      <h2 id="kbc-save-workspace-title" className="confirm__title">
        Save workspace — {repo}
      </h2>
      <form className="confirm__body kbc-save-workspace" onSubmit={(e) => void submit(e)}>
        <label className="kbc-save-workspace__field">
          <span>Name</span>
          <input
            type="text"
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoFocus
            aria-label="workspace name"
            data-kbc-save-workspace-name
          />
        </label>
        <label className="kbc-save-workspace__field">
          <span>Description (optional)</span>
          <textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            aria-label="workspace description"
            data-kbc-save-workspace-description
          />
        </label>
        <label className="kbc-save-workspace__field">
          <span>Ref</span>
          <input
            type="text"
            value={gitRef}
            onChange={(e) => setGitRef(e.target.value)}
            placeholder="branch or tag (optional)"
            aria-label="workspace ref"
            data-kbc-save-workspace-ref
          />
        </label>
        <p className="kbc-save-workspace__hint" data-kbc-save-workspace-files>
          {files.length} file{files.length === 1 ? "" : "s"} in the working set will be saved, in order.
        </p>
        <div className="confirm__actions">
          <button type="button" className="confirm__cancel" onClick={onClose} disabled={create.isPending}>
            Cancel
          </button>
          <button
            type="submit"
            className="confirm__go"
            disabled={!name.trim() || create.isPending}
            data-kbc-save-workspace-submit
          >
            {create.isPending ? "Saving…" : "Save workspace"}
          </button>
        </div>
      </form>
    </dialog>
  );
}
