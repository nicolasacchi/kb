import { useEffect, useRef, useState } from "react";
import { registerOpenFileForLiveMirror } from "../api/queryClient";
import { decideLiveMirrorAction, eventNamesOpenFile } from "../lib/liveMirror";

export interface HeadMovedInfo {
  old: string | null;
  new: string;
}

export interface LiveMirrorState {
  /// `true` once a `mirror.updated` for the OPEN file arrived while the
  /// viewer was dirty — drives the "file changed on disk — refresh"
  /// toast/badge (`lib/liveMirror.ts`'s heuristic).
  fileChangedOnDisk: boolean;
  /// Set on `repo.head_moved` for this repo — the top banner + its
  /// refresh-tree action.
  headMoved: HeadMovedInfo | null;
  dismissFileChanged: () => void;
  dismissHeadMoved: () => void;
}

/// W4.5's live-mirror UI. Registers `(repo, path)` + a `viewerDirty` probe
/// with `api/queryClient.ts`'s SSE bridge (`registerOpenFileForLiveMirror`)
/// so the bridge's own broad per-repo invalidation knows to skip
/// force-refetching THIS file when the viewer is mid-scroll/mid-selection
/// (see that module's doc), and listens for the two `kbc:*` `CustomEvent`s
/// the bridge re-dispatches:
///
/// - `kbc:mirror.updated` naming the open file → `decideLiveMirrorAction`
///   (`lib/liveMirror.ts`) decides: a pristine viewer calls `onSilentRefresh`
///   (the caller's own `useFile` query `refetch`) immediately; a dirty one
///   instead flips `fileChangedOnDisk` for the toast/badge, leaving the
///   stale-but-unfetched query alone until the user opts in.
/// - `kbc:repo.head_moved` for this repo → `headMoved` (the banner).
///
/// `viewerDirtyRef` is a plain ref (not state) — `CodeView`'s scroll/
/// selection tracking mutates it on every scroll/selection tick, which
/// would be far too hot a path to run through React state/re-renders; only
/// read at the moment an event actually arrives.
///
/// Wave E — `paneId` (e.g. `"pane1"`/`"pane2"`) is this hook instance's key
/// into `api/queryClient.ts`'s now-keyed registry (`registerOpenFileFor
/// LiveMirror`'s Map, one entry per open pane rather than one shared slot) —
/// `Reader.tsx` calls this hook once per pane, so BOTH panes' open files are
/// protected from a silent refetch while dirty, independently.
export function useLiveMirror(
  paneId: string,
  repo: string | undefined,
  path: string | undefined,
  viewerDirtyRef: { current: boolean },
  onSilentRefresh: () => void,
): LiveMirrorState {
  const [fileChangedOnDisk, setFileChangedOnDisk] = useState(false);
  const [headMoved, setHeadMoved] = useState<HeadMovedInfo | null>(null);
  const onSilentRefreshRef = useRef(onSilentRefresh);
  onSilentRefreshRef.current = onSilentRefresh;

  useEffect(() => {
    setFileChangedOnDisk(false);
    if (repo === undefined || path === undefined) return undefined;
    return registerOpenFileForLiveMirror(paneId, repo, path, () => viewerDirtyRef.current);
    // `viewerDirtyRef` is a stable ref object for the component's lifetime —
    // only `repo`/`path` (and `paneId`, stable per call site) should
    // re-register.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneId, repo, path]);

  useEffect(() => {
    function onMirrorUpdated(e: Event) {
      const detail = (e as CustomEvent<{ repo?: string; paths?: string[] } | null>).detail;
      if (!detail || !repo || !path || detail.repo !== repo) return;
      if (!eventNamesOpenFile(detail.paths ?? [], path)) return;
      const decision = decideLiveMirrorAction({
        fileChanged: true,
        viewerDirty: viewerDirtyRef.current,
      });
      if (decision === "auto-refresh") {
        setFileChangedOnDisk(false);
        onSilentRefreshRef.current();
      } else if (decision === "prompt") {
        setFileChangedOnDisk(true);
      }
    }
    function onHeadMovedEvent(e: Event) {
      const detail = (e as CustomEvent<{ repo?: string; old?: string | null; new?: string } | null>)
        .detail;
      if (!detail || !repo || detail.repo !== repo || !detail.new) return;
      setHeadMoved({ old: detail.old ?? null, new: detail.new });
    }
    window.addEventListener("kbc:mirror.updated", onMirrorUpdated);
    window.addEventListener("kbc:repo.head_moved", onHeadMovedEvent);
    return () => {
      window.removeEventListener("kbc:mirror.updated", onMirrorUpdated);
      window.removeEventListener("kbc:repo.head_moved", onHeadMovedEvent);
    };
  }, [repo, path, viewerDirtyRef]);

  return {
    fileChangedOnDisk,
    headMoved,
    dismissFileChanged: () => setFileChangedOnDisk(false),
    dismissHeadMoved: () => setHeadMoved(null),
  };
}
