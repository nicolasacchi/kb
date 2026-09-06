import { Icon } from "./icons";

function displayRef(r: string): string {
  return /^[0-9a-f]{40}$/i.test(r) ? r.slice(0, 7) : r;
}

export interface FileChangedToastProps {
  onRefresh: () => void;
  onDismiss: () => void;
  /// Wave E — which pane's file changed ("Pane 1"/"Pane 2"), named only
  /// when a split is actually open (`Reader.tsx` passes `undefined` in the
  /// single-pane case, keeping the plain pre-Wave-E wording when there's
  /// only one file open to begin with — no ambiguity to resolve).
  paneLabel?: string;
}

/// W4.5 — "file changed on disk — refresh": the dirty-viewer half of
/// `useLiveMirror`'s heuristic (`lib/liveMirror.ts`). A pristine viewer
/// never renders this at all — it silently refetches instead (see that
/// hook's doc) — so this toast only ever appears when swapping content out
/// from under the user would actually cost them something.
export function FileChangedToast({ onRefresh, onDismiss, paneLabel }: FileChangedToastProps) {
  return (
    <div className="kbc-livemirror-toast" role="status" data-kbc-file-changed-toast>
      <span>{paneLabel ? `${paneLabel}: file changed on disk` : "File changed on disk"}</span>
      <button type="button" className="kbc-livemirror-toast__refresh" onClick={onRefresh} data-kbc-file-changed-refresh>
        Refresh
      </button>
      <button
        type="button"
        className="kbc-livemirror-toast__dismiss"
        onClick={onDismiss}
        aria-label="dismiss"
      >
        <Icon.X />
      </button>
    </div>
  );
}

export interface HeadMovedBannerProps {
  newRef: string;
  onRefreshTree: () => void;
  onDismiss: () => void;
}

/// W4.5 — the top banner for `repo.head_moved`: unlike the file-changed
/// toast, a HEAD move always renders (it changes what "the current ref"
/// resolves to for every open view, not just the open file) — the
/// refresh-tree action re-fetches the tree/refs/repos queries for this
/// repo.
export function HeadMovedBanner({ newRef, onRefreshTree, onDismiss }: HeadMovedBannerProps) {
  return (
    <div className="kbc-headmoved-banner" role="status" data-kbc-headmoved-banner>
      <span>
        HEAD moved to <code>{displayRef(newRef)}</code>
      </span>
      <button type="button" onClick={onRefreshTree} data-kbc-headmoved-refresh>
        Refresh tree
      </button>
      <button
        type="button"
        className="kbc-headmoved-banner__dismiss"
        onClick={onDismiss}
        aria-label="dismiss"
      >
        <Icon.X />
      </button>
    </div>
  );
}
