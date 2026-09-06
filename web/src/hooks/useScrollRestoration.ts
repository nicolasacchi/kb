import { useEffect, useRef } from "react";

const PREFIX = "kb:scroll:";
// Bound the post-mount restore retry so it can NEVER become an idle loop
// (architecture invariant §2 in spirit): once the document is tall enough we
// restore and stop; if the content never grows we give up after ~1s of frames.
const MAX_RESTORE_FRAMES = 60;
const EMPTY_EPHEMERAL: readonly string[] = [];

// W3.D/S2 — the `ephemeralParams` amendment (invariant #31): strip the named
// query params from the STORAGE key only — the URL itself is untouched, and
// the stored offset still restores to whatever the live URL shows. This is
// deliberately NOT a general "ignore these params" escape hatch: the key
// still derives PURELY from the URL (no new component state), it just
// canonicalises a caller-declared set of params that toggle ephemeral UI
// (e.g. the sessions route's `?focus=`, which opens/closes the mobile sheet)
// rather than genuinely re-scoping the scrollable content. Pure + a plain
// string return so the hook below can depend on it as one primitive value
// regardless of whether the caller passes a fresh array literal each render.
export function normalizeScrollKey(
  rawKey: string,
  ephemeralParams: readonly string[],
): string {
  if (ephemeralParams.length === 0) return rawKey;
  const qIdx = rawKey.indexOf("?");
  if (qIdx === -1) return rawKey;
  const path = rawKey.slice(0, qIdx);
  const params = new URLSearchParams(rawKey.slice(qIdx + 1));
  let changed = false;
  for (const p of ephemeralParams) {
    if (params.has(p)) {
      params.delete(p);
      changed = true;
    }
  }
  if (!changed) return rawKey;
  const qs = params.toString();
  return qs ? `${path}?${qs}` : path;
}

// Per-tab scroll restoration keyed on the full URL (path + query). The gallery
// and search views scroll the *window* (useWindowVirtualizer / a plain result
// list — no inner overflow container), so a single window offset is the whole
// story. We:
//   • track the live offset in a passive `scroll` listener (it only wakes while
//     the user scrolls — no standing timer),
//   • persist it on unmount + `pagehide` (return-from-detail / hard reload),
//   • restore it when the same URL re-mounts, retrying per-frame until the
//     virtualizer/list has laid out enough height to actually reach the target.
// sessionStorage (not localStorage) so two tabs on different views never fight
// and the slot clears when the tab closes.
//
// The `dirty` guard is load-bearing: if the user navigates away *before* the
// restore lands (no scroll happened), we must NOT overwrite the saved slot with
// the current 0 — only persist once a real (or restored) scroll has occurred.
//
// `ephemeralParams` (W3.D/S2, optional, default none — every pre-W3 call site
// is unaffected): params in this list are stripped from the KEY only (see
// `normalizeScrollKey`), so a param that toggles ephemeral UI on top of a
// stable scrollable view (e.g. `/sessions?focus=`, which opens the mobile
// bottom sheet) doesn't fragment one scroll position into N slots.
export function useScrollRestoration(
  key: string,
  ephemeralParams: readonly string[] = EMPTY_EPHEMERAL,
) {
  const yRef = useRef(0);
  const dirtyRef = useRef(false);
  const normalizedKey = normalizeScrollKey(key, ephemeralParams);

  useEffect(() => {
    const storeKey = PREFIX + normalizedKey;
    yRef.current = window.scrollY;
    dirtyRef.current = false;

    let raf = 0;
    const saved = sessionStorage.getItem(storeKey);
    const target = saved === null ? NaN : Number(saved);
    if (Number.isFinite(target) && target > 0) {
      let frames = 0;
      const tryRestore = () => {
        const maxScroll =
          document.documentElement.scrollHeight - window.innerHeight;
        if (maxScroll >= target || frames++ >= MAX_RESTORE_FRAMES) {
          window.scrollTo(0, target); // fires `scroll` → flips dirty
        } else {
          raf = requestAnimationFrame(tryRestore);
        }
      };
      raf = requestAnimationFrame(tryRestore);
    }

    const onScroll = () => {
      yRef.current = window.scrollY;
      dirtyRef.current = true;
    };
    const persist = () => {
      if (dirtyRef.current) {
        sessionStorage.setItem(storeKey, String(Math.round(yRef.current)));
      }
    };

    window.addEventListener("scroll", onScroll, { passive: true });
    window.addEventListener("pagehide", persist);

    return () => {
      cancelAnimationFrame(raf);
      window.removeEventListener("scroll", onScroll);
      window.removeEventListener("pagehide", persist);
      persist();
    };
    // `normalizedKey` (a plain string) is the correct dependency — it already
    // reflects both `key` and the CONTENTS of `ephemeralParams`, so a caller
    // passing a fresh array literal each render doesn't retrigger this effect
    // unless the normalized key actually changed.
  }, [normalizedKey]);
}
