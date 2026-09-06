import { useCallback, useEffect, useState } from "react";
import {
  deleteSavedQuery,
  fetchSavedQueries,
  upsertSavedQuery,
} from "../api/client";
import { toast } from "../lib/toast";

// v0.12 Q4-lite → v0.13 Q4 full.
//
// Browser-local cache (`kb:saved-queries` in localStorage) + daemon-
// wide canonical store at `<state>/saved-queries.json`. The hook
// returns the localStorage list immediately on first render so the
// UI never blocks, then overlays the daemon's list once the GET
// resolves. Writes go to both sides (localStorage is updated
// synchronously for the optimistic UI; the daemon write happens in
// the background — failures fall back to a localStorage-only
// success so the user isn't penalised for an offline daemon).

export type SavedQuery = {
  /// User-chosen short name. Unique by case-insensitive label.
  name: string;
  /// The full `location.search` (including the leading `?`) at save time.
  /// Restoring is a single navigate to `path + search`.
  search: string;
  /// `location.pathname` so a saved gallery query restores to `/` and
  /// a saved memory filter restores to `/memory`.
  path: string;
  /// Unix epoch seconds when the row was saved.
  saved_at: number;
};

const KEY = "kb:saved-queries";

function read(): SavedQuery[] {
  if (typeof localStorage === "undefined") return [];
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return [];
    const arr = JSON.parse(raw);
    if (!Array.isArray(arr)) return [];
    return arr.filter(
      (q): q is SavedQuery =>
        q &&
        typeof q.name === "string" &&
        typeof q.search === "string" &&
        typeof q.path === "string" &&
        typeof q.saved_at === "number",
    );
  } catch {
    return [];
  }
}

function write(qs: SavedQuery[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(qs));
    // Notify same-tab subscribers — `storage` events fire only on
    // *other* tabs, so we dispatch our own custom event for the
    // current document.
    window.dispatchEvent(new CustomEvent("kb:saved-queries-changed"));
  } catch {
    // localStorage might be denied (private window / quota). Saves
    // silently fail — the UI state still updates this turn; refresh
    // would lose it.
  }
}

/// `save`'s optional second argument — a caller that already knows the
/// exact `path`/`search` it wants saved (rather than the current
/// `window.location`). W3 C-c's reflection-canvas "save as scene" chip is
/// the first caller: the canvas's brush/track selection is pure CLIENT
/// state, deliberately never written into the URL bar (see
/// ReflectionCanvas.tsx's header note), so `window.location.search` alone
/// can't capture it — the caller builds the restorable search string
/// itself and hands it in here.
export type SaveOverride = { path: string; search: string };

export type UseSavedQueriesResult = {
  queries: SavedQuery[];
  save: (name: string, override?: SaveOverride) => boolean;
  remove: (name: string) => void;
  /// True iff there's any saved query whose pathname + search match the
  /// current location (useful for the "already saved" affordance).
  matchesCurrent: () => SavedQuery | undefined;
};

export function useSavedQueries(): UseSavedQueriesResult {
  const [queries, setQueries] = useState<SavedQuery[]>(() => read());

  // localStorage cache sync (cross-tab + same-tab via custom event).
  useEffect(() => {
    const reload = () => setQueries(read());
    window.addEventListener("storage", reload);
    window.addEventListener("kb:saved-queries-changed", reload);
    return () => {
      window.removeEventListener("storage", reload);
      window.removeEventListener("kb:saved-queries-changed", reload);
    };
  }, []);

  // v0.13 Q4 — overlay the daemon's canonical list when reachable.
  // Daemon wins (so devices share their saved queries via the
  // daemon's <state>/saved-queries.json), then mirror back to
  // localStorage so the next render's instant read matches.
  useEffect(() => {
    const ctl = new AbortController();
    fetchSavedQueries(ctl.signal)
      .then((r) => {
        const remote: SavedQuery[] = r.queries.map((q) => ({
          name: q.name,
          path: q.path,
          search: q.search,
          saved_at: q.saved_at,
        }));
        // Only mirror when the remote list differs — avoids a wasted
        // localStorage write + storage event on every page load.
        const local = read();
        if (JSON.stringify(local) !== JSON.stringify(remote)) {
          write(remote);
        }
        setQueries(remote);
      })
      .catch(() => {
        // Daemon offline / route unknown — stay on the localStorage
        // cache; the user keeps working.
      });
    return () => ctl.abort();
  }, []);

  const save = useCallback((name: string, override?: SaveOverride) => {
    const trimmed = name.trim();
    if (!trimmed) return false;
    const cur = read();
    const lower = trimmed.toLowerCase();
    // De-dupe by case-insensitive name: overwrite the existing row
    // so users get a "rename" path for free.
    const filtered = cur.filter((q) => q.name.toLowerCase() !== lower);
    const fresh: SavedQuery = {
      name: trimmed,
      search: override?.search ?? window.location.search,
      path: override?.path ?? window.location.pathname,
      saved_at: Math.floor(Date.now() / 1000),
    };
    filtered.unshift(fresh);
    write(filtered);
    // Best-effort daemon sync — local already updated, so this can't
    // poison the UX by blocking on it, but #32 still means the failure
    // must reach the user rather than vanish silently.
    void upsertSavedQuery(fresh.name, fresh.path, fresh.search)
      .then((r) => {
        const remote: SavedQuery[] = r.queries.map((q) => ({
          name: q.name,
          path: q.path,
          search: q.search,
          saved_at: q.saved_at,
        }));
        if (JSON.stringify(read()) !== JSON.stringify(remote)) {
          write(remote);
        }
      })
      .catch(() =>
        toast.err(
          "saved locally, but couldn't sync to the daemon (it may be offline)",
        ),
      );
    return true;
  }, []);

  const remove = useCallback((name: string) => {
    const cur = read();
    const lower = name.toLowerCase();
    const next = cur.filter((q) => q.name.toLowerCase() !== lower);
    write(next);
    void deleteSavedQuery(name).catch(() =>
      toast.err("Couldn't delete saved query"),
    );
  }, []);

  const matchesCurrent = useCallback((): SavedQuery | undefined => {
    if (typeof window === "undefined") return undefined;
    const here = window.location.pathname;
    const hereSearch = window.location.search;
    return queries.find((q) => q.path === here && q.search === hereSearch);
  }, [queries]);

  return { queries, save, remove, matchesCurrent };
}
