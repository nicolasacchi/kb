import { useEffect, useRef, useState } from "react";
import * as ws from "../lib/workingSet";
import type { WorkingSetEntry } from "../lib/workingSet";

export interface WorkingSetHandle {
  entries: WorkingSetEntry[];
  touch: (path: string) => void;
  pin: (path: string) => void;
  unpin: (path: string) => void;
  remove: (path: string) => void;
  /// V71-K2 — lay the strip out in this order. A workspace RESTORE is the
  /// only caller; see `lib/workingSet.ts`'s `reorder` for why the `touch`
  /// loop it replaces could never have restored the saved order.
  reorder: (paths: readonly string[]) => void;
  /// `[f`/`]f` — the next/previous path relative to `current`, or `null` if
  /// the working set is empty. Pure lookup (doesn't itself mutate state).
  cycle: (current: string | undefined, dir: -1 | 1) => string | null;
}

/// React binding for `lib/workingSet.ts`'s pure transforms: one working set
/// PER REPO, `sessionStorage`-persisted (`kbc:ws:<repo>`) — `Reader.tsx`
/// calls this once, keyed on the route's own `repo` param, and both panes'
/// file-open effects call `touch()` into the SAME instance.
export function useWorkingSet(repo: string): WorkingSetHandle {
  const [state, setState] = useState<ws.WorkingSetState>(() => ws.loadWorkingSet(repo));
  const repoRef = useRef(repo);

  // A repo switch loads THAT repo's own persisted set fresh — working sets
  // don't merge or carry over across repos (mirrors `pane2`'s own
  // same-repo-only scope, `lib/codeUrl.ts`'s `PaneLoc`).
  useEffect(() => {
    if (repoRef.current === repo) return;
    repoRef.current = repo;
    setState(ws.loadWorkingSet(repo));
  }, [repo]);

  useEffect(() => {
    ws.saveWorkingSet(repo, state);
  }, [repo, state]);

  return {
    entries: state.entries,
    touch: (path) => setState((s) => ws.touch(s, path)),
    pin: (path) => setState((s) => ws.pin(s, path)),
    unpin: (path) => setState((s) => ws.unpin(s, path)),
    remove: (path) => setState((s) => ws.remove(s, path)),
    reorder: (paths) => setState((s) => ws.reorder(s, paths)),
    cycle: (current, dir) => ws.cycle(state, current, dir),
  };
}
