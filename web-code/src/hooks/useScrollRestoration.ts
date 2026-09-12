// V70-A6 — scroll restoration, ported from kb's `web/src/hooks/
// useScrollRestoration.ts` (root CLAUDE.md invariant #31) rather than
// re-invented (D25: "kb's keymap.ts and useScrollRestoration ported rather
// than re-invented").
//
// The recon's G1 was blunt: kb-code had NONE of this. `scrollRestoration`
// appeared nowhere in `web-code/src`, `main.tsx` never set it to `"manual"`,
// and every long list page — `~reviews`, `~todos`, `~hotspots`, `/search`,
// `~branches`, `~sets` — landed at whatever offset the browser guessed on
// Back. The reader was the one surface with an answer, and only because the
// debounced cursor sync had written `?line=`.
//
// WHAT IS PORTED VERBATIM (invariant #31's own text):
//   * the key is the FULL URL (`pathname + search`), normalised by
//     `normalizeScrollKey` against a caller-declared ephemeral-param list;
//   * `sessionStorage`, not `localStorage` — two tabs on different views must
//     never fight, and the slot dies with the tab;
//   * a bounded per-frame retry (`MAX_RESTORE_FRAMES`), so waiting for a
//     virtualiser to lay out can never become a standing render loop;
//   * the `dirty` guard — navigating away BEFORE the restore lands must not
//     overwrite the saved slot with the current 0.
//
// WHAT KB-CODE HAD TO CHANGE, and it is load-bearing: **kb-code's list pages
// do not scroll the WINDOW.** kb's SPA does (its gallery/search are plain
// window-scrolled lists), so kb's hook reads `window.scrollY` throughout. Here
// `.kbc-app` is `height: 100vh` and `.kbc-approute` is `overflow: auto`
// (`styles/reader.css`), so `window.scrollY` is permanently 0 and a
// straight port would have been a no-op that LOOKED like a feature. The hook
// therefore resolves its target at effect time: the app scroller when one is
// mounted, the window otherwise (which keeps it correct for any future
// window-scrolled surface, and for a unit environment with no shell at all).
//
// WHAT KB-CODE ADDS: a per-ENTRY slot when the Navigation API is available.
// `entry.key` names a history SLOT, and two visits to the same URL get
// different keys — so Back onto a list you have visited twice restores the
// offset you left THAT time, which a URL key structurally cannot do. Where
// the Navigation API is absent (`nav/history.ts`'s History adapter) the key
// is the normalised URL, exactly as in kb. Both paths are in the same hook so
// a caller never has to know which browser it is in.

import { useEffect, useRef } from "react";
import { useLocation as useRouterLocation } from "react-router";
import { historyAdapter } from "../nav/history";

const PREFIX = "kbc:scroll:";
const ENTRY_PREFIX = "kbc:scroll:key:";
// Bound the post-mount restore retry so it can NEVER become an idle loop:
// once the document is tall enough we restore and stop; if the content never
// grows we give up after ~1s of frames.
const MAX_RESTORE_FRAMES = 60;
const EMPTY_EPHEMERAL: readonly string[] = [];

/// Strip the named query params from the STORAGE key only — the URL itself is
/// untouched, and the stored offset still restores to whatever the live URL
/// shows. Deliberately NOT a general "ignore these params" escape hatch: the
/// key still derives PURELY from the URL, it just canonicalises a
/// caller-declared set of params that toggle ephemeral UI (kb-code's cases:
/// `?focus=`, `?thread=`, `?finding=` — each opens a panel or flashes a row
/// on top of an unchanged scrollable list) rather than genuinely re-scoping
/// the scrollable content.
export function normalizeScrollKey(rawKey: string, ephemeralParams: readonly string[]): string {
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

/// The ephemeral params every kb-code list page declares. One shared constant
/// because all six pages have the same three: a row focus, a thread deep
/// link and a finding deep link — none of which changes what is scrollable.
export const LIST_EPHEMERAL_PARAMS: readonly string[] = ["focus", "thread", "finding"];

/// kb-code's app shell scroller (`styles/reader.css`'s `.kbc-approute`). See
/// the module doc for why this is not `window`.
const APP_SCROLLER = ".kbc-approute";

interface ScrollTarget {
  get(): number;
  set(y: number): void;
  /// The largest offset that can currently be reached — the bounded retry
  /// waits for this to grow past the target before restoring.
  max(): number;
  on(cb: () => void): () => void;
}

/// Resolve the scrollable thing. Read fresh inside the effect (never cached
/// at module level): the shell mounts after this module is imported, and a
/// route swap can replace the element.
function resolveScrollTarget(): ScrollTarget {
  const el = typeof document === "undefined" ? null : document.querySelector<HTMLElement>(APP_SCROLLER);
  if (el) {
    return {
      get: () => el.scrollTop,
      set: (y) => {
        el.scrollTop = y;
      },
      max: () => el.scrollHeight - el.clientHeight,
      on: (cb) => {
        el.addEventListener("scroll", cb, { passive: true });
        return () => el.removeEventListener("scroll", cb);
      },
    };
  }
  return {
    get: () => window.scrollY,
    set: (y) => window.scrollTo(0, y),
    max: () => document.documentElement.scrollHeight - window.innerHeight,
    on: (cb) => {
      window.addEventListener("scroll", cb, { passive: true });
      return () => window.removeEventListener("scroll", cb);
    },
  };
}

/// Per-tab scroll restoration keyed on the full URL (path + query), or on the
/// history ENTRY key where the platform has one. See the module doc.
export function useScrollRestoration(
  key: string,
  ephemeralParams: readonly string[] = EMPTY_EPHEMERAL,
) {
  const yRef = useRef(0);
  const dirtyRef = useRef(false);
  const normalizedKey = normalizeScrollKey(key, ephemeralParams);

  useEffect(() => {
    // The entry key is read at MOUNT (i.e. after the traversal has landed),
    // which is precisely when `navigation.currentEntry` names the slot we are
    // restoring. Falling back to the URL key is not a degradation of
    // correctness, only of precision: two visits to one URL then share a slot.
    const entryKey = historyAdapter().currentKey();
    const storeKey = entryKey ? ENTRY_PREFIX + entryKey : PREFIX + normalizedKey;
    // The URL slot is written ALONGSIDE the entry slot when both exist, so a
    // reload (which mints a fresh entry key) still restores something.
    const urlKey = PREFIX + normalizedKey;
    const t = resolveScrollTarget();
    yRef.current = t.get();
    dirtyRef.current = false;

    let raf = 0;
    const saved = sessionStorage.getItem(storeKey) ?? sessionStorage.getItem(urlKey);
    const target = saved === null ? NaN : Number(saved);
    if (Number.isFinite(target) && target > 0) {
      let frames = 0;
      const tryRestore = () => {
        if (t.max() >= target || frames++ >= MAX_RESTORE_FRAMES) {
          t.set(target); // fires `scroll` → flips dirty
        } else {
          raf = requestAnimationFrame(tryRestore);
        }
      };
      raf = requestAnimationFrame(tryRestore);
    }

    const onScroll = () => {
      yRef.current = t.get();
      dirtyRef.current = true;
    };
    const persist = () => {
      if (!dirtyRef.current) return;
      const v = String(Math.round(yRef.current));
      sessionStorage.setItem(storeKey, v);
      if (storeKey !== urlKey) sessionStorage.setItem(urlKey, v);
    };

    const offScroll = t.on(onScroll);
    window.addEventListener("pagehide", persist);

    return () => {
      cancelAnimationFrame(raf);
      offScroll();
      window.removeEventListener("pagehide", persist);
      persist();
    };
    // `normalizedKey` (a plain string) is the correct dependency — it already
    // reflects both `key` and the CONTENTS of `ephemeralParams`, so a caller
    // passing a fresh array literal each render doesn't retrigger this effect
    // unless the normalized key actually changed.
  }, [normalizedKey]);
}

/// The one-liner every list page calls. Keys on the live URL and declares
/// kb-code's three ephemeral params — so a route only has to say "I am a
/// list", not restate the key derivation.
export function useListScrollRestoration(): void {
  const loc = useRouterLocation();
  useScrollRestoration(loc.pathname + loc.search, LIST_EPHEMERAL_PARAMS);
}
