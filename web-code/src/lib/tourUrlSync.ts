// Debounced, pure (no React) sync of tour mode's current step index into
// the reader URL's `step=` query param, via `history.replaceState` — the
// exact same "state updates immediately, the URL bookkeeping is debounced"
// discipline as `storyUrlSync.ts`'s `createStoryUrlSync` (this player's own
// analogue: a plain 1-based step NUMBER instead of a commit sha, `step=`
// instead of `at=`). Kept as its own tiny module rather than generalizing
// `storyUrlSync.ts` into a shared abstraction — the two are independent,
// single-purpose helpers (one per player), and ~30 lines of debounce
// plumbing isn't worth the indirection a shared generic would add.
//
// Debounced (not per-keystroke) for the same reason story mode's sync is:
// ArrowLeft/ArrowRight can repeat rapidly while held.

export interface TourUrlSyncOptions {
  /// Debounce window in ms. Defaults to 250 — matches `storyUrlSync.ts`'s
  /// own default.
  debounceMs?: number;
  /// Caller-supplied `history.replaceState` wiring. Receives the FULL new
  /// `search` string in `location.search`'s own convention: `""` when there
  /// are no query params, otherwise a leading `?`.
  replace: (search: string) => void;
  /// Caller-supplied accessor for the CURRENT `location.search` — read
  /// fresh at flush time (not at `onStep` time), so a param changed by
  /// something else in between is respected rather than clobbered.
  getSearch: () => string;
}

export interface TourUrlSync {
  /// Report the current 0-based step index. Debounced — only the last call
  /// within `debounceMs` actually reaches the URL, serialized 1-based
  /// (`step=1` is the first span — human-friendly for a shared link).
  onStep(index: number): void;
  /// Cancel any pending debounced write. Safe to call multiple times.
  dispose(): void;
}

const DEFAULT_DEBOUNCE_MS = 250;

export function createTourUrlSync(opts: TourUrlSyncOptions): TourUrlSync {
  const debounceMs = opts.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function flush(index: number): void {
    const params = new URLSearchParams(opts.getSearch());
    const value = String(index + 1);
    if (params.get("step") === value) return; // no-op — unchanged
    params.set("step", value);
    const qs = params.toString();
    opts.replace(qs ? `?${qs}` : "");
  }

  return {
    onStep(index) {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        flush(index);
      }, debounceMs);
    },
    dispose() {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    },
  };
}
