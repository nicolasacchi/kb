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

/**
 * The device's own short side from `screen`, or null when it is not reported
 * (jsdom, some privacy modes). Unlike the viewport it does not shrink when a
 * soft keyboard opens, and `min` makes it immune to orientation quirks (iOS
 * keeps `screen.height` in portrait terms).
 */
export function deviceShortSide(scr: { availWidth?: number; availHeight?: number } | undefined): number | null {
  const w = scr?.availWidth;
  const h = scr?.availHeight;
  if (typeof w !== "number" || typeof h !== "number" || !(w > 0) || !(h > 0)) return null;
  return Math.min(w, h);
}

// The height arm of the query is evaluated on the CURRENT viewport height, which
// a soft keyboard shrinks: a coarse landscape tablet (800 tall) with the keyboard
// up is 400 tall and would flip to the phone branch mid-typing. Latch it on the
// larger of the viewport height and the device's short side: only a device whose
// own short side is under PHONE_SHORT_SIDE takes the height arm.
export function phoneByHeight(viewportHeight: number, shortSide: number | null): boolean {
  return Math.max(viewportHeight, shortSide ?? 0) < PHONE_SHORT_SIDE;
}

const WIDTH_QUERY = `(max-width: ${MOBILE_MAX_WIDTH}px)`;

export function usePhonePanel(): boolean {
  const matched = useMatches(PHONE_PANEL_QUERY);
  const narrow = useMatches(WIDTH_QUERY);
  if (!matched) return false;
  if (narrow) return true;
  if (typeof window === "undefined") return true;
  return phoneByHeight(window.innerHeight, deviceShortSide(window.screen));
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
