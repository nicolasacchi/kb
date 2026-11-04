import { useEffect, useRef } from "react";

/**
 * Answer the platform close request (Android system Back / swipe-back, and Esc)
 * for a modal surface that is open.
 *
 * Failure this prevents: on a phone, pressing Back with a bottom sheet or
 * drawer open left the artifact (or exited the installed standalone PWA)
 * instead of dismissing the sheet. A `CloseWatcher` makes Back close the
 * surface first, and it is never a history entry: nothing is pushed or popped,
 * so the reader's one-entry-per-artifact history and scroll keys are untouched.
 *
 * Chrome on Android routes Back only to the FOCUSED frame's watchers. A tap
 * inside the cross-origin artifact iframe leaves that iframe focused, so a
 * watcher in this document would never see Back. When the hook activates it
 * blurs a focused `<iframe>` so the request reaches this document.
 *
 * Without the API (Safari, iOS, older Chrome) the hook is a strict no-op: no
 * watcher, no listener, no focus change. No watcher exists while `active` is
 * false, so Back with nothing open still navigates as before.
 *
 * `onClose` must make `active` false. It is idempotent with the surface's own
 * Esc handler: a keypress that reaches both paths just closes twice. The
 * capture-phase keydown listener only observes; it never prevents or stops a
 * keydown, so Esc reaches its owners (editors, vim layers) untouched.
 */
export type CloseRequestOptions = {
  /** Return true for an Escape keydown that must NOT dismiss (typing targets, vim buffers, modifier chords).
   *  Receives the keydown itself so callers can copy their existing guard exactly. */
  ignoreEscape?: (e: KeyboardEvent) => boolean;
};

interface CloseWatcherLike {
  oncancel: ((e: Event) => void) | null;
  onclose: ((e: Event) => void) | null;
  requestClose(): void;
  close(): void;
  destroy(): void;
}
type CloseWatcherCtor = new () => CloseWatcherLike;

function closeWatcherCtor(): CloseWatcherCtor | null {
  if (typeof window === "undefined") return null;
  const c = (window as unknown as { CloseWatcher?: unknown }).CloseWatcher;
  return typeof c === "function" ? (c as CloseWatcherCtor) : null;
}

export function useCloseRequest(
  active: boolean,
  onClose: () => void,
  opts?: CloseRequestOptions,
): void {
  const onCloseRef = useRef(onClose);
  const optsRef = useRef(opts);
  onCloseRef.current = onClose;
  optsRef.current = opts;

  useEffect(() => {
    if (!active) return;
    const Ctor = closeWatcherCtor();
    if (!Ctor) return;
    let disposed = false;
    let escIgnored = false;
    let watcher: CloseWatcherLike | null = null;
    let clearFlag: ReturnType<typeof setTimeout> | undefined;
    let rearm: ReturnType<typeof setTimeout> | undefined;

    const arm = () => {
      if (disposed || watcher) return;
      const w = new Ctor();
      w.oncancel = (e) => {
        if (escIgnored && e.cancelable) e.preventDefault();
      };
      w.onclose = () => {
        if (watcher === w) watcher = null;
        if (disposed) return;
        if (escIgnored) {
          // A typed Esc with no activation cannot be cancelled: re-arm, never close.
          rearm ??= setTimeout(() => {
            rearm = undefined;
            arm();
          }, 0);
          return;
        }
        onCloseRef.current();
      };
      watcher = w;
    };

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const ignore = optsRef.current?.ignoreEscape;
      escIgnored = !!ignore && ignore(e);
      clearTimeout(clearFlag);
      // The close request runs in the keydown's own task.
      clearFlag = setTimeout(() => {
        escIgnored = false;
      }, 0);
    };

    arm();
    window.addEventListener("keydown", onKeyDown, true);
    const ae = document.activeElement as HTMLElement | null;
    if (ae && ae.tagName === "IFRAME") ae.blur();

    return () => {
      disposed = true;
      window.removeEventListener("keydown", onKeyDown, true);
      clearTimeout(clearFlag);
      clearTimeout(rearm);
      const w = watcher;
      watcher = null;
      try {
        w?.destroy();
      } catch {
        /* already closed by the platform: destroy is a no-op, never a throw we surface */
      }
    };
  }, [active]);
}
