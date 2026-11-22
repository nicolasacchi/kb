// FUTERMFULL — full screen for the terminal panel (desktop) and the phone
// root. Pure decision logic plus thin DOM glue, injectable for tests.
//
// Native element fullscreen is used only where Esc can stay with the
// terminal: when `navigator.keyboard.lock` exists (Chromium) the panel is
// fullscreened and Escape is locked, so a short Esc reaches the agent and
// holding it exits natively. Everywhere else (Firefox, desktop Safari, a
// missing API, a rejected promise) the panel is MAXIMISED by a CSS class. The
// overlay binds no Escape and never uses CloseWatcher: Esc always reaches the
// agent, and the exit is the visible button.
//
// State is derived, never stored: `document.fullscreenElement === panel` OR
// the overlay class. Full screen never sends a grid change.

export const MAX_CLASS = "kb-term--max";

export interface FsPanel {
  requestFullscreen?: (options?: { navigationUI?: "hide" | "show" | "auto" }) => Promise<void>;
  classList: { add(c: string): void; remove(c: string): void; contains(c: string): boolean };
}

export interface FsDoc {
  fullscreenElement: unknown;
  fullscreenEnabled?: boolean;
  exitFullscreen?: () => Promise<void>;
  addEventListener(type: "fullscreenchange", cb: () => void): void;
  removeEventListener(type: "fullscreenchange", cb: () => void): void;
}

export interface FsKeyboard {
  lock?: (keys: string[]) => Promise<void>;
  unlock?: () => void;
}

export interface FsNav {
  keyboard?: FsKeyboard;
}

/** Native full screen is wanted only when Escape can be locked (Esc stays with the terminal). */
export function nativeUsable(nav: FsNav | undefined, doc: Pick<FsDoc, "fullscreenEnabled">, panel: FsPanel): boolean {
  return (
    typeof nav?.keyboard?.lock === "function" &&
    doc.fullscreenEnabled !== false &&
    typeof panel.requestFullscreen === "function"
  );
}

/**
 * The phone's button: element fullscreen must exist (Android Chrome); there is
 * no overlay there, the 100dvh sheet already is the maximum (iPhone Safari has
 * no element fullscreen, so no button).
 */
export function phoneFullscreenAvailable(
  doc: Pick<FsDoc, "fullscreenEnabled"> & { documentElement?: Partial<FsPanel> },
): boolean {
  return doc.fullscreenEnabled === true && typeof doc.documentElement?.requestFullscreen === "function";
}

export interface FsOptions {
  panel: FsPanel;
  doc: FsDoc;
  nav?: FsNav;
  /** Called with the DERIVED state after every change. */
  onChange: (on: boolean) => void;
  /** Phone: native only; when native is unusable `enter` does nothing. Default true. */
  overlay?: boolean;
  /** Phone: no Escape lock and native fullscreen whenever the API exists. Default true. */
  lockEscape?: boolean;
}

export interface FsControl {
  enter(): Promise<void>;
  exit(): Promise<void>;
  /** The derived state. */
  active(): boolean;
  /** Leave everything: native fullscreen, the key lock and the overlay class. */
  dispose(): void;
}

export function createFullscreen(o: FsOptions): FsControl {
  const { panel, doc, nav } = o;
  const overlayAllowed = o.overlay !== false;
  const lockEscape = o.lockEscape !== false;
  let locked = false;
  let disposed = false;
  // A fast double toggle must not ask the browser twice while the first request is pending.
  let entering = false;

  const native = (): boolean => doc.fullscreenElement === panel;
  const active = (): boolean => native() || panel.classList.contains(MAX_CLASS);

  const unlock = (): void => {
    if (!locked) return;
    locked = false;
    try {
      nav?.keyboard?.unlock?.();
    } catch {
      // best effort
    }
  };
  const lock = (): void => {
    if (!lockEscape || locked || typeof nav?.keyboard?.lock !== "function") return;
    locked = true;
    try {
      void nav.keyboard.lock(["Escape"]).catch(() => {
        locked = false;
      });
    } catch {
      locked = false;
    }
  };

  const onFsChange = (): void => {
    if (disposed) return;
    if (native()) lock();
    else unlock();
    o.onChange(active());
  };
  doc.addEventListener("fullscreenchange", onFsChange);

  const useOverlay = (): void => {
    panel.classList.add(MAX_CLASS);
    o.onChange(active());
  };

  return {
    active,
    async enter() {
      if (disposed || active() || entering) return;
      const usable = lockEscape
        ? nativeUsable(nav, doc, panel)
        : doc.fullscreenEnabled !== false && typeof panel.requestFullscreen === "function";
      if (!usable) {
        if (overlayAllowed) useOverlay();
        return;
      }
      entering = true;
      try {
        await panel.requestFullscreen!({ navigationUI: "hide" });
        // The change event drives the lock and the state; a browser that
        // resolves without firing one still gets it.
        if (native()) onFsChange();
      } catch {
        if (overlayAllowed) useOverlay();
      } finally {
        entering = false;
      }
    },
    async exit() {
      if (native()) {
        try {
          await doc.exitFullscreen?.();
        } catch {
          // the change event (or dispose) settles it
        }
      }
      unlock();
      panel.classList.remove(MAX_CLASS);
      o.onChange(active());
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      doc.removeEventListener("fullscreenchange", onFsChange);
      if (native()) {
        try {
          void doc.exitFullscreen?.()?.catch(() => {});
        } catch {
          // best effort
        }
      }
      unlock();
      panel.classList.remove(MAX_CLASS);
    },
  };
}
