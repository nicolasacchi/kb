import { useEffect, useState } from "react";

const KEY_PREFIX = "kbc:activeWorkspace:";

function storageKey(repo: string): string {
  return `${KEY_PREFIX}${repo}`;
}

function readStorage(repo: string): string | null {
  try {
    return sessionStorage.getItem(storageKey(repo));
  } catch {
    return null;
  }
}

export interface ActiveWorkspaceHandle {
  id: string | null;
  set: (id: string | null) => void;
}

/// V70-A10 ("Workspaces v0") — the workspace CURRENTLY considered "open"
/// for `repo`, per TAB (`sessionStorage`, same per-repo scoping
/// `hooks/useWorkingSet.ts` already uses for the working set itself).
/// `?workspace=<id>` on the URL (the one-shot restore trigger,
/// `Reader.tsx`'s workspace-open effect) is what SETS this; it then
/// SURVIVES subsequent same-tab navigation that drops the URL param (a
/// plain file-tree click doesn't carry `?workspace=` forward — that param
/// is deliberately NOT part of `lib/codeUrl.ts`'s permanent grammar, the
/// same "one-shot override" posture `?desk=` already takes,
/// `desk/useDesk.ts`'s doc), so the strip's chip and the rail's "Workspace
/// notes" section stay live while browsing within the workspace. Never
/// auto-clears — closing/switching happens by opening a DIFFERENT
/// workspace (overwrites) or clearing the working set entirely (a future
/// unit's call, not this one's).
export function useActiveWorkspace(repo: string): ActiveWorkspaceHandle {
  const [id, setId] = useState<string | null>(() => readStorage(repo));

  // A repo switch re-seeds from THAT repo's own persisted value — mirrors
  // `useWorkingSet`'s own repo-switch effect.
  useEffect(() => {
    setId(readStorage(repo));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo]);

  function set(next: string | null) {
    setId(next);
    try {
      if (next) sessionStorage.setItem(storageKey(repo), next);
      else sessionStorage.removeItem(storageKey(repo));
    } catch {
      // best-effort — a denied/full sessionStorage just doesn't persist.
    }
  }

  return { id, set };
}
