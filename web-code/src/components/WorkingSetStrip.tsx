import { Link } from "react-router-dom";
import { splitPath, type WorkingSetEntry } from "../lib/workingSet";
import { Icon } from "./icons";

/// V70-A10 ("Workspaces v0", D26) — the currently open workspace (from
/// `?workspace=` on the URL), for the strip's leading chip. `dirty` is
/// whether the LIVE working-set path list (order + membership) differs
/// from the workspace's own saved entries — computed by `Reader.tsx`
/// (`lib/workspaceDirty.ts`'s `isWorkingSetDirty`), not this component.
export interface ActiveWorkspaceChip {
  id: string;
  name: string;
  url: string;
  dirty: boolean;
}

export interface WorkingSetStripProps {
  entries: WorkingSetEntry[];
  /// The two panes' own open paths (pane2 `undefined` when no split is
  /// open) — a chip whose path matches either is highlighted "active".
  pane1Path: string | undefined;
  pane2Path: string | undefined;
  /// A plain click opens into the FOCUSED pane; a middle-click or
  /// Shift+click opens into pane2 regardless of focus (mirrors the file
  /// tree's own Shift+Enter/Shift+click grammar).
  onOpen: (path: string, target: "focused" | "pane2") => void;
  onPin: (path: string) => void;
  onUnpin: (path: string) => void;
  onRemove: (path: string) => void;
  /// V70-A10 — `null`/`undefined` when no workspace is open (the strip's
  /// pre-A10 markup, byte-identical).
  activeWorkspace?: ActiveWorkspaceChip | null;
  /// Re-save the CURRENT working set (order + per-open-pane lines) over
  /// the same workspace id. Omitted while no workspace is open.
  onUpdateWorkspace?: () => void;
  /// Open the "Save workspace" dialog pre-filled as a NEW workspace.
  onSaveWorkspaceAs?: () => void;
}

/// Wave E — the slim strip between the reader's top chrome and its body:
/// one chip per recently/pinned-open file (`lib/workingSet.ts`), basename
/// prominent + dirname dimmed, a pin toggle and a "✕" remove revealed on
/// hover (kept visually quiet — see `reader.css`'s doc), the chip(s)
/// matching either open pane highlighted. Renders nothing when the working
/// set is empty AND no workspace is open (`Reader.tsx` only mounts this
/// component on reader routes to begin with, so "only on reader routes,
/// only when there's something to show" both hold without this component
/// needing route awareness of its own).
///
/// V70-A10 adds the leading workspace chip (name, links to `~workspaces`,
/// a dirty marker + "update"/"save as" actions when the live working set
/// has drifted from the workspace's own saved entries) — see
/// `ActiveWorkspaceChip`'s doc.
export default function WorkingSetStrip({
  entries,
  pane1Path,
  pane2Path,
  onOpen,
  onPin,
  onUnpin,
  onRemove,
  activeWorkspace = null,
  onUpdateWorkspace,
  onSaveWorkspaceAs,
}: WorkingSetStripProps) {
  if (entries.length === 0 && !activeWorkspace) return null;

  return (
    <div className="kbc-ws" role="tablist" aria-label="Working set">
      {activeWorkspace && (
        <div
          className={"kbc-ws-workspace-chip" + (activeWorkspace.dirty ? " is-dirty" : "")}
          data-kbc-ws-workspace-chip={activeWorkspace.id}
        >
          <Link to={activeWorkspace.url} className="kbc-ws-workspace-chip__name" data-kbc-ws-workspace-link>
            <Icon.Layers />
            {activeWorkspace.name}
          </Link>
          {activeWorkspace.dirty && (
            <>
              <span className="kbc-ws-workspace-chip__dirty" data-kbc-ws-workspace-dirty title="unsaved changes">
                ·
              </span>
              {onUpdateWorkspace && (
                <button
                  type="button"
                  className="kbc-ws-workspace-chip__update"
                  onClick={onUpdateWorkspace}
                  data-kbc-ws-workspace-update
                >
                  Update
                </button>
              )}
              {onSaveWorkspaceAs && (
                <button
                  type="button"
                  className="kbc-ws-workspace-chip__save-as"
                  onClick={onSaveWorkspaceAs}
                  data-kbc-ws-workspace-save-as
                >
                  Save as…
                </button>
              )}
            </>
          )}
        </div>
      )}
      {entries.map((e) => {
        const { dir, base } = splitPath(e.path);
        const isPane1 = e.path === pane1Path;
        const isPane2 = e.path === pane2Path;
        const pinned = e.pinnedAt !== null;
        return (
          <div
            key={e.path}
            role="tab"
            aria-selected={isPane1 || isPane2}
            className={
              "kbc-ws-chip" +
              (isPane1 || isPane2 ? " is-active" : "") +
              (pinned ? " is-pinned" : "")
            }
            data-kbc-ws-chip={e.path}
            title={e.path}
            tabIndex={0}
            onClick={(ev) => {
              onOpen(e.path, ev.shiftKey ? "pane2" : "focused");
            }}
            onAuxClick={(ev) => {
              if (ev.button === 1) {
                ev.preventDefault();
                onOpen(e.path, "pane2");
              }
            }}
            onKeyDown={(ev) => {
              if (ev.key === "Enter") onOpen(e.path, ev.shiftKey ? "pane2" : "focused");
            }}
          >
            {dir !== "" && <span className="kbc-ws-chip__dir">{dir}/</span>}
            <span className="kbc-ws-chip__base">{base}</span>
            <button
              type="button"
              className="kbc-ws-chip__pin"
              data-kbc-ws-pin={e.path}
              aria-label={pinned ? `Unpin ${e.path}` : `Pin ${e.path}`}
              title={pinned ? "Unpin" : "Pin"}
              onClick={(ev) => {
                ev.stopPropagation();
                (pinned ? onUnpin : onPin)(e.path);
              }}
            >
              <Icon.Pin />
            </button>
            <button
              type="button"
              className="kbc-ws-chip__x"
              data-kbc-ws-remove={e.path}
              aria-label={`Remove ${e.path} from the working set`}
              title="Remove"
              onClick={(ev) => {
                ev.stopPropagation();
                onRemove(e.path);
              }}
            >
              <Icon.X />
            </button>
          </div>
        );
      })}
    </div>
  );
}
