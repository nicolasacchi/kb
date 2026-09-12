// V70-A6 — THE door. Every SPA navigation goes through here (§P7).
//
// Before this module the SPA moved in a dozen different ways: React Router's
// `navigate()` with a `lib/codeUrl.ts` string, `navigate()` with a hand-built
// template literal (recon R4, two of them), `history.replaceState` from three
// separate debounced URL syncers, a `<Link to>`, and `window.open`. Each one
// decided independently whether it was a push or a replace, which is why Back
// meant something different depending on which sub-widget last touched the
// URL — the second of the three things the operator's "travelling
// continuously without hard stops" bar rules out.
//
// The rule this module enforces: **the push/replace decision is
// `nav/location.ts`'s `transition` table and nothing else.** A caller says
// WHERE and WHY; it never says how. The one escape hatch, `as`, exists for
// the two cases where the table cannot know (a `?sym=` resolution replacing
// its own landing URL, and a redirect route) and is spelled out at each use.
//
// WHAT STAYS: React Router. `BrowserRouter` in `main.tsx` still owns
// rendering, and a push/replace still goes through its `useNavigate()` — the
// history adapter (`nav/history.ts`) never pushes behind React Router's back,
// because a raw `history.pushState` would move the URL without re-rendering
// the route. The adapter owns TRAVERSAL (`back`/`forward`), entry keys and
// the navigate-event subscription; React Router owns push, replace and
// rendering. Two owners, one for each half, with no overlap.

import { useCallback, useMemo, useRef } from "react";
import { useLocation as useRouterLocation, useNavigate } from "react-router";
import { recordJump, type PaneId } from "../lib/navHistory";
import type { TrailVia } from "../lib/codeUrl";
import { historyAdapter } from "./history";
import { decode, encode, transition, type Location, type Transition } from "./location";

/// Why a navigation happened. The typed `via` vocabulary plus the reasons
/// that are not edges (a traversal, a URL sync, a redirect). Recorded on the
/// jump ring and, for a `TrailVia`, on the trail.
export type NavReason = TrailVia | "back" | "forward" | "jump" | "url-sync" | "redirect";

export function isTrailVia(reason: NavReason): reason is TrailVia {
  return (
    reason !== "back" &&
    reason !== "forward" &&
    reason !== "jump" &&
    reason !== "url-sync" &&
    reason !== "redirect"
  );
}

export interface NavigateOptions {
  /// Override the transition table. Only two callers may use this, and both
  /// say why at the call site: a `?sym=` resolution (which rewrites the URL
  /// it just landed on) and a redirect route (`FindingEntry`).
  as?: Transition;
  /// Record this landing on the pane's jump ring. Omit for a move that is not
  /// a jump (a view-tab change, a cursor sync).
  record?: { pane: PaneId; snippet?: string };
}

export interface Navigator {
  /// Go somewhere. `target` may be a `Location` or a URL a `lib/codeUrl.ts`
  /// builder already produced — the URL form is decoded first, so the
  /// transition table sees the same value either way.
  go(target: Location | string, reason: NavReason, opts?: NavigateOptions): void;
  /// In-app Back IS the browser's Back (§P7: "there is no second stack").
  back(): void;
  forward(): void;
  /// The location the browser is at right now, decoded.
  current(): Location;
  /// Which history adapter is live — surfaced so a surface can be honest
  /// about what it can restore.
  readonly adapterKind: "navigation" | "history";
}

/// The one navigator. A hook (not a module singleton) because the push half
/// must go through React Router's `useNavigate`, which only exists inside the
/// router.
export function useNavigator(focusedPane: 1 | 2 = 1): Navigator {
  const routerNavigate = useNavigate();
  const routerLoc = useRouterLocation();
  const adapter = historyAdapter();
  // The location we last navigated FROM. Kept in a ref rather than derived
  // from `routerLoc` alone so the transition table sees the value we actually
  // left, including the pane focus and overlay set the URL never carries.
  const lastRef = useRef<Location | null>(null);

  const currentUrl = routerLoc.pathname + routerLoc.search;
  const current = useMemo(() => decode(currentUrl, focusedPane), [currentUrl, focusedPane]);

  const go = useCallback(
    (target: Location | string, reason: NavReason, opts?: NavigateOptions) => {
      const to = typeof target === "string" ? decode(target, focusedPane) : target;
      const from = lastRef.current ?? decode(currentUrl, focusedPane);
      const how = opts?.as ?? transition(from, to);
      lastRef.current = to;
      if (opts?.record && to.path) {
        recordJump({
          repo: to.repo,
          path: to.path,
          line: to.anchor?.line ?? 1,
          snippet: opts.record.snippet ?? "",
          pane: opts.record.pane,
          ...(isTrailVia(reason) ? { via: reason } : {}),
        });
      }
      if (how === "none") return;
      routerNavigate(encode(to), { replace: how === "replace" });
    },
    [currentUrl, focusedPane, routerNavigate],
  );

  return useMemo<Navigator>(
    () => ({
      go,
      back: () => adapter.back(),
      forward: () => adapter.forward(),
      current: () => current,
      adapterKind: adapter.kind,
    }),
    [go, adapter, current],
  );
}
