import { useEffect } from "react";

// SH.B2 — shared body-scroll lock for the mobile sheets/drawers
// (MobileDrawer, the detail route's reader-tools sheet, the sessions
// inspector sheet). Each of these used to toggle
// `document.body.style.overflow` inline (MobileDrawer.tsx's own effect was
// the original copy); pulled out here so every caller locks/unlocks the
// SAME way, and — the reason it can't just be "set hidden on mount, restore
// on unmount" per caller — so two sheets that are momentarily both mounted
// (e.g. a mode switch that mounts the next sheet a frame before the
// previous one's close transition finishes) don't fight over the restore
// value: the FIRST locker to mount captures the pre-lock `overflow` and the
// LAST one to unmount restores it, via a simple module-level counter rather
// than each instance's own effect closure guessing what the "real" prior
// value was.
let lockCount = 0;
let previousOverflow: string | null = null;

/**
 * Locks `document.body`'s scroll (via `overflow: hidden`) for as long as
 * `active` is true. Nests: concurrent lockers share one counter, so the
 * body is only unlocked once the last active caller releases it, and the
 * ORIGINAL inline `overflow` value (captured once, by whichever caller
 * locks first) is what gets restored — never a stale `"hidden"` left behind
 * by a sibling that unlocked first.
 */
export function useBodyScrollLock(active: boolean): void {
  useEffect(() => {
    if (!active) return;
    if (lockCount === 0) {
      previousOverflow = document.body.style.overflow;
      document.body.style.overflow = "hidden";
    }
    lockCount += 1;
    return () => {
      lockCount -= 1;
      if (lockCount === 0) {
        document.body.style.overflow = previousOverflow ?? "";
        previousOverflow = null;
      }
    };
  }, [active]);
}
