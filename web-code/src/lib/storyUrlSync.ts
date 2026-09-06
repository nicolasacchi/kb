// Debounced, pure (no React) sync of the story player's current step sha
// into the reader URL's `at=` query param, via `history.replaceState` — same
// "state updates immediately, the URL bookkeeping is debounced"
// discipline as `cursorUrlSync.ts`'s `createCursorUrlSync` (this player's
// own analogue: a single sha instead of a line range, `at=` instead of
// `line=`). Debounced (not per-keystroke) because ArrowLeft/ArrowRight can
// repeat rapidly while held — the step itself must feel instant, but the
// URL doesn't need a `replaceState` call for every intermediate step a fast
// key-repeat skips past.

export interface StoryUrlSyncOptions {
  /// Debounce window in ms between the last `onStep` call and the `replace`
  /// call it produces. Defaults to 250 (shorter than `cursorUrlSync`'s
  /// 500ms — a step is a discrete, already-deliberate action, not a
  /// continuously-updating cursor position).
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

export interface StoryUrlSync {
  /// Report the current step's commit sha. Debounced — only the last call
  /// within `debounceMs` actually reaches the URL.
  onStep(sha: string): void;
  /// Cancel any pending debounced write. Safe to call multiple times.
  dispose(): void;
}

const DEFAULT_DEBOUNCE_MS = 250;

export function createStoryUrlSync(opts: StoryUrlSyncOptions): StoryUrlSync {
  const debounceMs = opts.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function flush(sha: string): void {
    const params = new URLSearchParams(opts.getSearch());
    if (params.get("at") === sha) return; // no-op — unchanged
    params.set("at", sha);
    const qs = params.toString();
    opts.replace(qs ? `?${qs}` : "");
  }

  return {
    onStep(sha) {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        flush(sha);
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
