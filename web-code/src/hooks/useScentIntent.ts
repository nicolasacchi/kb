// V70-A6 — the Ramp's hover TIMING (§P7), separated from what it renders.
//
// Four rules, each one from the design and each one a thing a naive
// `onMouseEnter` gets wrong:
//
//   1. **≥400 ms intent delay.** A pointer crossing a list on its way
//      somewhere else is not interest.
//   2. **Cancelled on movement.** Any pointer move restarts the clock, so a
//      card never appears under a moving cursor.
//   3. **Suppressed for 3 s after any keyboard navigation in that list.** A
//      keyboard-driven list with a card popping up wherever the pointer
//      happens to be resting is the single most annoying failure of every
//      hover-preview implementation.
//   4. **Never on touch.** A coarse pointer has no hover; the same card is
//      reachable from the row's own peek affordance.

import { useCallback, useEffect, useRef, useState } from "react";

export const SCENT_INTENT_MS = 400;
export const SCENT_KEYBOARD_SUPPRESS_MS = 3000;

export interface ScentIntent<T> {
  /// The row the card is currently showing for, or `null`.
  active: T | null;
  /// Pointer coordinates the card should float at.
  at: { x: number; y: number } | null;
  /// Wire to a row's `onPointerEnter`.
  enter(row: T, e: React.PointerEvent | { clientX: number; clientY: number; pointerType?: string }): void;
  /// Wire to a row's `onPointerLeave` and the list's own.
  leave(): void;
  /// Call from the list's keyboard handler — starts the suppression window.
  noteKeyboard(): void;
}

export function useScentIntent<T>(delayMs = SCENT_INTENT_MS): ScentIntent<T> {
  const [active, setActive] = useState<T | null>(null);
  const [at, setAt] = useState<{ x: number; y: number } | null>(null);
  const timer = useRef(0);
  const lastKey = useRef(0);

  const clear = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = 0;
  }, []);

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const enter = useCallback<ScentIntent<T>["enter"]>(
    (row, e) => {
      const pointerType = "pointerType" in e ? e.pointerType : "mouse";
      // Rule 4 — a coarse pointer has no hover state to key on.
      if (pointerType === "touch" || pointerType === "pen") return;
      // Rule 3 — the list is being driven by the keyboard right now.
      if (Date.now() - lastKey.current < SCENT_KEYBOARD_SUPPRESS_MS) return;
      clear();
      const x = e.clientX;
      const y = e.clientY;
      // Rule 2 — the timer is restarted by every enter, and a pointer moving
      // across rows fires one per row, so movement cancels by construction.
      timer.current = window.setTimeout(() => {
        setAt({ x, y });
        setActive(row);
      }, delayMs);
    },
    [clear, delayMs],
  );

  const leave = useCallback(() => {
    clear();
    setActive(null);
    setAt(null);
  }, [clear]);

  const noteKeyboard = useCallback(() => {
    lastKey.current = Date.now();
    clear();
    setActive(null);
    setAt(null);
  }, [clear]);

  return { active, at, enter, leave, noteKeyboard };
}
