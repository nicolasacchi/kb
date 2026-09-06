// Debounced, pure (no React) sync of the CM6 cursor/selection into the
// reader URL's `line=` query param, via `history.replaceState` (never
// `pushState` — a cursor move is not a navigation event). Pure so it's
// trivially unit-testable with fake timers; the owning hook/component
// wires `replace`/`getSearch` to `window.history`/`window.location`.
//
// Uses `formatLineParam`/`parseLineParam` from ./codeUrl so the `line=`
// grammar this module writes is byte-identical to the one `codeUrl` reads
// and builds — one grammar, shared by both the deep-link builder and the
// live cursor-tracking writer.
//
// Wave E adds `createPane2CursorUrlSync`, a sibling factory (same
// `onSelection`/`dispose` shape) for the second pane: its cursor drives the
// `:line` suffix INSIDE the `pane2=` query value (`lib/codeUrl.ts`'s
// `formatPane2`/`parsePane2` grammar) rather than a plain top-level param.

import { formatLineParam, formatPane2, parsePane2 } from "./codeUrl";

export interface CursorUrlSyncOptions {
  /// Debounce window in ms between the last `onSelection` call and the
  /// `replace` call it produces. Defaults to 500.
  debounceMs?: number;
  /// Caller-supplied `history.replaceState` wiring. Receives the FULL new
  /// `search` string in `location.search`'s own convention: `""` when
  /// there are no query params, otherwise a leading `?`.
  replace: (search: string) => void;
  /// Caller-supplied accessor for the CURRENT `location.search` — read
  /// fresh at flush time (not at `onSelection` time), so a param changed
  /// by something else in between is respected rather than clobbered.
  getSearch: () => string;
}

export interface CursorUrlSync {
  /// Report the current selection (or `null` to clear it). Debounced —
  /// only the last call within `debounceMs` actually reaches the URL.
  onSelection(sel: { start: number; end: number } | null): void;
  /// Cancel any pending debounced write. Safe to call multiple times.
  dispose(): void;
}

const DEFAULT_DEBOUNCE_MS = 500;

export function createCursorUrlSync(opts: CursorUrlSyncOptions): CursorUrlSync {
  const debounceMs = opts.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function flush(sel: { start: number; end: number } | null): void {
    const params = new URLSearchParams(opts.getSearch());
    const nextLine = sel ? formatLineParam(sel) : "";
    const currentLine = params.get("line") ?? "";
    if (nextLine === currentLine) return; // no-op — serialized value unchanged

    if (nextLine === "") {
      params.delete("line");
    } else {
      params.set("line", nextLine);
    }
    const qs = params.toString();
    opts.replace(qs ? `?${qs}` : "");
  }

  return {
    onSelection(sel) {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        flush(sel);
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

// --- Wave E — the second pane's own cursor → `pane2=` sync ----------------

export interface Pane2CursorUrlSyncOptions extends CursorUrlSyncOptions {
  /// The pane2 location's `path`/`ref` (never its `line` — that's exactly
  /// what `onSelection` supplies on every call). Read fresh at FLUSH time
  /// (not at sync-creation time, and not at `onSelection` time) via a
  /// getter, so a `path`/`ref` change elsewhere (`[f ]f` cycling pane2 to a
  /// different file, or a fresh `Ctrl-w v` split) between the last
  /// `onSelection` and the debounce firing is respected. Returning `null`
  /// means "pane2 isn't open any more" — the pending write is dropped
  /// rather than resurrecting a closed pane's `pane2=` param.
  getPaneBase: () => { path: string; ref?: string } | null;
}

/// Sibling of `createCursorUrlSync` for the second pane: same debounce-
/// then-`history.replaceState` discipline, but rewrites only the TRAILING
/// `:line` suffix of the `pane2=` value (`lib/codeUrl.ts`'s `formatPane2`
/// grammar) — `path`/`ref` are supplied by `getPaneBase`, never touched by
/// this sync itself.
export function createPane2CursorUrlSync(opts: Pane2CursorUrlSyncOptions): CursorUrlSync {
  const debounceMs = opts.debounceMs ?? DEFAULT_DEBOUNCE_MS;
  let timer: ReturnType<typeof setTimeout> | null = null;

  function flush(sel: { start: number; end: number } | null): void {
    const base = opts.getPaneBase();
    if (!base) return; // pane2 closed mid-debounce — nothing to write

    const params = new URLSearchParams(opts.getSearch());
    const current = parsePane2(params.get("pane2"));
    // Stale-write guard: only ever rewrite a pane2 param that's STILL this
    // same path/ref — a debounced flush racing a navigation that changed
    // (or closed) pane2 must not clobber the newer state.
    if (!current || current.path !== base.path || (current.ref ?? undefined) !== (base.ref ?? undefined)) {
      return;
    }

    const nextLine = sel ? formatLineParam(sel) : "";
    const currentLine = current.line !== undefined ? formatLineParam(current.line) : "";
    if (nextLine === currentLine) return; // no-op — serialized value unchanged

    const next = formatPane2({ path: base.path, ref: base.ref, line: sel ?? undefined });
    params.set("pane2", next);
    const qs = params.toString();
    opts.replace(qs ? `?${qs}` : "");
  }

  return {
    onSelection(sel) {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        flush(sel);
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
