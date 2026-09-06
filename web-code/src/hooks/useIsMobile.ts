import { useEffect, useState } from "react";

// F5 ("kb-code v2 — The Operable Reader", mobile shell) — ported from kb's
// own `web/src/hooks/useIsMobile.ts` (root CLAUDE.md invariant #23: mobile is
// ONE bottom sheet at ≤860px, `asSheet`-gated so desktop DOM stays untouched).
// Reader's three-column layout (a 260px tree + a 220px outline rail either
// side of the CM6 buffer) needs materially more than 860px to lay out
// side-by-side without crushing the code column, so the SAME breakpoint kb
// settled on for its own (denser) header cluster applies here too — no
// kb-code-specific tuning needed.
//
// Keep this literal in sync with the `860px` used throughout
// `styles/mobile.css` — CSS custom properties can't be referenced inside a
// media-query condition, so the breakpoint is duplicated by necessity.
export const MOBILE_MAX_WIDTH = 860;

// Reactive `matchMedia` wrapper. Used for the mobile branches that must
// happen in JS (promoting the tree aside to a `MobileDrawer`, the inspector
// aside to a bottom sheet, closing the tree drawer on file-select); chrome
// hide/show is done in CSS to avoid a hydration flash.
export function useIsMobile(maxWidth: number = MOBILE_MAX_WIDTH): boolean {
  const query = `(max-width: ${maxWidth}px)`;
  const [isMobile, setIsMobile] = useState<boolean>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(query).matches
      : false,
  );

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function")
      return;
    const mql = window.matchMedia(query);
    const onChange = () => setIsMobile(mql.matches);
    onChange();
    mql.addEventListener("change", onChange);
    return () => mql.removeEventListener("change", onChange);
  }, [query]);

  return isMobile;
}
