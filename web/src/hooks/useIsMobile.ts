import { useEffect, useState } from "react";

// Mobile-shell breakpoint. The desktop header's control cluster (workspace +
// anchor + search + 7-tab view strip + saved-queries + theme + settings)
// needs ~790px to lay out; below ~860px it overflows and the fixed 240px
// sidebar crushes the content column, so the mobile shell (hamburger drawer +
// compact header + immersive reading) takes over.
//
// Keep this literal in sync with the `860px` used throughout
// styles/mobile.css — CSS custom properties can't be referenced inside a
// media-query condition, so the breakpoint is duplicated by necessity.
export const MOBILE_MAX_WIDTH = 860;

// Reactive `matchMedia` wrapper. Used for the few mobile branches that must
// happen in JS (mounting the drawer/sheets, conditional rails, atlas touch
// wiring); chrome hide/show is done in CSS to avoid a hydration flash.
export function useIsMobile(maxWidth: number = MOBILE_MAX_WIDTH): boolean {
  return useMatches(`(max-width: ${maxWidth}px)`);
}

// A touch device whose short side is under this is a phone even when its long
// side passes the breakpoint (a phone held in landscape is about 915x412).
export const PHONE_SHORT_SIDE = 600;

// The terminal panel's own predicate: the width breakpoint OR a coarse pointer
// with a short viewport height. Panel-only; the app shell keeps `useIsMobile`.
// A touch tablet in landscape (height >= 600) and any fine-pointer window stay
// on the desktop panel.
export const PHONE_PANEL_QUERY = `(max-width: ${MOBILE_MAX_WIDTH}px), (pointer: coarse) and (max-height: ${PHONE_SHORT_SIDE - 1}px)`;

export function usePhonePanel(): boolean {
  return useMatches(PHONE_PANEL_QUERY);
}

function useMatches(query: string): boolean {
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
