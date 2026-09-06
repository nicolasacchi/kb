import { useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { fetchSet } from "../api/client";
import type { SetGroupOut, SetSummary } from "../api/types";
import EmptyState from "../components/EmptyState";
import { Icon } from "../components/icons";
import { useConfirm } from "../components/ConfirmProvider";
import { useCreateSet, useDeleteSet, usePatchSet, useWorkspaceGroups } from "../hooks/useSets";
import { codeUrl } from "../lib/codeUrl";
import { relativeTime } from "../lib/format";
import { toast } from "../lib/toast";
import { parseWorkspaceSnapshot } from "../lib/workspaceSnapshot";
import "../styles/sets.css";

/// Append `?workspace=<id>` (or `&workspace=<id>` when the URL already has
/// a query string) to a reader URL — the ONE-SHOT restore trigger
/// `Reader.tsx`'s workspace-open effect reads on mount (same posture
/// `?desk=`'s one-shot preset override takes, `desk/useDesk.ts`'s doc) —
/// deliberately NOT part of `lib/codeUrl.ts`'s permanent `ref`/`line`/
/// `pane2` grammar.
function withWorkspaceParam(url: string, id: string): string {
  return url + (url.includes("?") ? "&" : "?") + `workspace=${encodeURIComponent(id)}`;
}

/// V70-A10 ("Workspaces v0", D26) — `/r/{repo}/~workspaces`: the
/// branch-view list of workspaces, grouped by `ref`. `?ref=<branch>`
/// (a `~branches` row's "N workspaces" chip) narrows to one group with a
/// "show all" escape hatch. Open navigates to the FIRST entry's reader URL
/// with `?workspace=<id>` (`Reader.tsx` restores the desk from there);
/// rename/duplicate/delete are row actions, delete via `useConfirm` (root
/// CLAUDE.md invariant #32 — never `window.confirm`).
export default function Workspaces() {
  const { repo = "" } = useParams<{ repo: string }>();
  const [searchParams, setSearchParams] = useSearchParams();
  const navigate = useNavigate();
  const groups = useWorkspaceGroups(repo);
  const patchWorkspace = usePatchSet(repo);
  const deleteWorkspace = useDeleteSet(repo);
  const createWorkspace = useCreateSet(repo);
  const confirm = useConfirm();

  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [duplicatingId, setDuplicatingId] = useState<string | null>(null);

  const refFilter = searchParams.get("ref");
  const allGroups = groups.data?.groups ?? [];
  const visibleGroups = refFilter ? allGroups.filter((g) => g.ref === refFilter) : allGroups;

  async function openWorkspace(w: SetSummary) {
    try {
      const view = await fetchSet(w.id);
      // The desk snapshot's own `pane1`/`pane2` name the EXACT file (+
      // line) that was open in each pane at save time — the precise
      // restore target. Falls back to the first saved span (e.g. a
      // CLI-`save`d workspace with no `desk_json` at all) when there is
      // none.
      const snapshot = view.desk_json ? parseWorkspaceSnapshot(view.desk_json) : null;
      const pane1 = snapshot?.pane1;
      const first = view.spans[0];
      const base = pane1
        ? codeUrl({ repo, path: pane1.path, line: pane1.line, pane2: snapshot?.pane2 ?? undefined })
        : first
          ? codeUrl({ repo, path: first.path, ref: first.ref, line: first.line_start })
          : codeUrl({ repo, path: "" });
      navigate(withWorkspaceParam(base, w.id));
    } catch (e) {
      toast.err(`couldn't open workspace: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  function startRename(w: SetSummary) {
    setRenamingId(w.id);
    setRenameDraft(w.name);
  }

  async function saveRename(id: string) {
    const n = renameDraft.trim();
    if (!n) return;
    try {
      await patchWorkspace.mutateAsync({ id, input: { name: n } });
      setRenamingId(null);
    } catch (e) {
      toast.err(`couldn't rename workspace: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function removeWorkspace(w: SetSummary) {
    const ok = await confirm({
      title: `Delete "${w.name}"?`,
      body: "This removes its notes too — this can't be undone.",
      confirmLabel: "Delete",
    });
    if (!ok) return;
    try {
      await deleteWorkspace.mutateAsync(w.id);
    } catch (e) {
      toast.err(`couldn't delete workspace: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  async function duplicateWorkspace(w: SetSummary) {
    setDuplicatingId(w.id);
    try {
      const view = await fetchSet(w.id);
      await createWorkspace.mutateAsync({
        repo,
        name: `${view.name} copy`,
        kind: "workspace",
        description: view.description ?? undefined,
        description_md: view.description_md,
        ref: view.ref,
        desk_json: view.desk_json,
        spans: view.spans.map((s) => ({
          path: s.path,
          line_start: s.line_start,
          line_end: s.line_end,
          ref: s.ref,
          note: s.note,
        })),
      });
      toast.ok(`duplicated "${view.name}"`);
    } catch (e) {
      toast.err(`couldn't duplicate workspace: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setDuplicatingId(null);
    }
  }

  return (
    <div className="kbc-sets" id="main">
      <header className="kbc-sets__head">
        <h1 className="kbc-sets__title">Workspaces — {repo}</h1>
        <p className="kbc-sets__hint">
          Saved open-file sets — the "Save workspace" action in the reader (
          <code>desk.save-workspace</code>) creates one.
        </p>
        {refFilter && (
          <button
            type="button"
            className="kbc-sets__row-name"
            onClick={() => setSearchParams({})}
            data-kbc-workspaces-clear-ref
          >
            showing only <code>{refFilter}</code> · show all
          </button>
        )}
      </header>

      {groups.isLoading ? (
        <div className="kbc-reader__hint">Loading workspaces…</div>
      ) : groups.error ? (
        <div className="kbc-reader__hint kbc-reader__hint--error">{(groups.error as Error).message}</div>
      ) : visibleGroups.length === 0 ? (
        <EmptyState
          icon={<Icon.Layers />}
          title="No workspaces yet"
          hint="Save one from the reader — open a few files, then Save workspace."
        />
      ) : (
        visibleGroups.map((g: SetGroupOut) => (
          <section key={g.ref ?? "(no ref)"} className="kbc-sets__group" data-kbc-workspace-group={g.ref ?? ""}>
            <h2 className="kbc-sets__group-title">{g.ref ?? "(no ref)"}</h2>
            <ul className="kbc-sets__list" data-kbc-workspaces-list>
              {g.workspaces.map((w) => (
                <li key={w.id} className="kbc-sets__row" data-kbc-workspace-row={w.id}>
                  {renamingId === w.id ? (
                    <form
                      className="kbc-sets__rename-form"
                      onSubmit={(e) => {
                        e.preventDefault();
                        void saveRename(w.id);
                      }}
                    >
                      <input
                        type="text"
                        value={renameDraft}
                        onChange={(e) => setRenameDraft(e.target.value)}
                        aria-label="rename workspace"
                        autoFocus
                        data-kbc-workspace-rename-input
                      />
                      <button type="submit" data-kbc-workspace-rename-save>
                        Save
                      </button>
                      <button type="button" onClick={() => setRenamingId(null)}>
                        Cancel
                      </button>
                    </form>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="kbc-sets__row-name"
                        onClick={() => void openWorkspace(w)}
                        data-kbc-workspace-open={w.id}
                      >
                        {w.name}
                      </button>
                      {w.description && <span className="kbc-sets__row-desc">{w.description}</span>}
                      <span className="kbc-sets__row-meta" data-kbc-workspace-meta={w.id}>
                        {w.span_count} file{w.span_count === 1 ? "" : "s"} · {w.note_count} note
                        {w.note_count === 1 ? "" : "s"} · updated {relativeTime(w.updated_at)}
                      </span>
                      <div className="kbc-sets__row-actions">
                        <button type="button" onClick={() => startRename(w)} data-kbc-workspace-rename={w.id}>
                          Rename
                        </button>
                        <button
                          type="button"
                          disabled={duplicatingId === w.id}
                          onClick={() => void duplicateWorkspace(w)}
                          data-kbc-workspace-duplicate={w.id}
                        >
                          {duplicatingId === w.id ? "Duplicating…" : "Duplicate"}
                        </button>
                        <button
                          type="button"
                          onClick={() => void removeWorkspace(w)}
                          data-kbc-workspace-delete={w.id}
                        >
                          Delete
                        </button>
                      </div>
                    </>
                  )}
                </li>
              ))}
            </ul>
          </section>
        ))
      )}
    </div>
  );
}
