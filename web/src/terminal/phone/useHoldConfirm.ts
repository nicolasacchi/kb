// T18 — press-and-hold for a key-bar button.
//
// Pointer handlers for a button that must be HELD for `ms` before it counts:
// Ctrl-C (600 ms: a stray tap must not interrupt the harness) and the `/`
// command sheet (450 ms). Releasing early cancels the hold and reports a plain
// tap; completing the hold fires `onHold` exactly once, and the release that
// follows is silent. Every handler calls `preventDefault()` on press: a key
// bar tap must never take focus away from the composer (focus pops the soft
// keyboard).

import { useCallback, useEffect, useRef } from "react";

export interface HoldHandlers {
  onPointerDown: (e: { preventDefault(): void }) => void;
  onMouseDown: (e: { preventDefault(): void }) => void;
  onPointerUp: (e: { preventDefault(): void }) => void;
  onPointerCancel: () => void;
  onPointerLeave: () => void;
}

export interface HoldOptions {
  /** The hold completed (the button was held for the full `ms`). */
  onHold: () => void;
  /** The press ended before `ms`: a quick tap. */
  onTap?: () => void;
}

export function useHoldConfirm(ms: number, opts: HoldOptions): HoldHandlers {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // "down": pressed, hold not yet complete; "held": hold fired; "idle".
  const phase = useRef<"idle" | "down" | "held">("idle");
  const optsRef = useRef(opts);
  optsRef.current = opts;

  const clear = useCallback((): void => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  }, []);

  useEffect(() => clear, [clear]);

  const onPointerDown = useCallback(
    (e: { preventDefault(): void }): void => {
      e.preventDefault();
      clear();
      phase.current = "down";
      timer.current = setTimeout(() => {
        timer.current = null;
        if (phase.current !== "down") return;
        phase.current = "held";
        optsRef.current.onHold();
      }, ms);
    },
    [clear, ms],
  );

  const onPointerUp = useCallback(
    (e: { preventDefault(): void }): void => {
      e.preventDefault();
      const was = phase.current;
      clear();
      phase.current = "idle";
      if (was === "down") optsRef.current.onTap?.();
    },
    [clear],
  );

  const cancel = useCallback((): void => {
    // A drag off the button or a system cancel: neither a tap nor a hold.
    clear();
    phase.current = "idle";
  }, [clear]);

  return {
    onPointerDown,
    onMouseDown: (e) => e.preventDefault(),
    onPointerUp,
    onPointerCancel: cancel,
    onPointerLeave: cancel,
  };
}
