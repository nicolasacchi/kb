// V70-A6 — resolve this tab's `?trail=&step=` into a renderable origin.
//
// Three resolution paths, tried in order, and an honest nothing at the end:
//
//   1. **This tab already has the trail.** A tab opened by the Ramp's TAB
//      rung (`window.open(url, "_blank")`, no `noopener`) inherits a COPY of
//      the opener's `sessionStorage` in the browsers kb-code targets, so the
//      common case resolves synchronously with no message at all.
//   2. **Ask the other tabs.** `lib/tabRegistry.ts` broadcasts a
//      `trail.request` on `kbc-tabs`; whoever holds the trail answers. This
//      is the path the WINDOW rung takes (`noopener` gives a fresh
//      `sessionStorage`), and it is bounded — 600 ms, then give up.
//   3. **Give up, render nothing.** No chip is better than a dead chip. v7.0
//      trails are browser-local by ruling; a truly cold open (another
//      device, a restored tab after the origin closed) genuinely cannot know,
//      and `kbc-trail/1` server trails are v7.4.

import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { parseTrailLink, type TrailLink } from "../lib/codeUrl";
import { joinTabRegistry, requestTrail, setTabTrail } from "../lib/tabRegistry";
import { loadTrail, type Trail } from "../lib/trail";

export interface TrailLinkState {
  /// The `?trail=&step=` triple, or `null` when this tab is not trail-linked.
  link: TrailLink | null;
  /// The resolved trail body, or `null` while resolving / after giving up.
  trail: Trail | null;
  /// True once resolution has finished, however it finished — so a caller can
  /// distinguish "still asking" from "asked, nobody knew".
  settled: boolean;
}

export function useTrailLink(): TrailLinkState {
  const [params] = useSearchParams();
  const raw = params.toString();
  const [state, setState] = useState<TrailLinkState>({ link: null, trail: null, settled: false });

  useEffect(() => {
    const link = parseTrailLink(new URLSearchParams(raw));
    if (!link) {
      setState({ link: null, trail: null, settled: true });
      return;
    }
    let live = true;
    const local = loadTrail(link.id);
    if (local) {
      setState({ link, trail: local, settled: true });
      return;
    }
    setState({ link, trail: null, settled: false });
    void requestTrail(link.id).then((ok) => {
      if (!live) return;
      setState({ link, trail: ok ? loadTrail(link.id) : null, settled: true });
    });
    return () => {
      live = false;
    };
  }, [raw]);

  // Announce which trail this tab is on, so the ORIGIN tab's chip can count
  // us. Idempotent, and a no-op where `BroadcastChannel` is missing.
  useEffect(() => {
    const leave = joinTabRegistry(state.link?.id ?? null);
    return leave;
    // Only on mount: `setTabTrail` below carries later changes, and
    // re-joining on every trail change would churn the channel.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    setTabTrail(state.link?.id ?? null);
  }, [state.link?.id]);

  return state;
}
