// V70-A6 — the Ramp (§P7): ONE commitment gradient, identical on every
// result surface.
//
//   hover        scent card — what will I find there, how big, have I been?
//   K            peek — look without committing
//   Enter        open in the focused pane
//   Shift-Enter  open in the other pane
//   Ctrl-Enter/o open in a new browser tab, TRAIL-LINKED
//   O            open in a new browser window, same link
//
// The gradient is the survey's single most transferable idea (Arc's Peek →
// Little Arc → Split View, VS Code's preview editors, JetBrains' preview
// tab): a reader must never have to decide "do I want to keep this?" BEFORE
// looking at it. The scent card is the Information-Foraging half — Piorkowski
// et al. measured that over 50% of navigation choices return less value than
// predicted, so cost has to be visible before the hop and retreat has to be
// cheap.
//
// ONE HANDLER, MANY SURFACES. Everything below is surface-agnostic: it takes
// a `RampTarget` (what the row points at) and does the same thing regardless
// of whether the row came from search, a peek list, a hierarchy tree, the
// impact panel, the file tree, the drawer, Recent Locations or a review
// thread. `ramp.test.ts` asserts that every one of those components imports
// THIS module — the mechanical version of "one home for one action".
//
// The registry rows are `ramp.open-tab` / `ramp.open-pane2` /
// `ramp.open-window` (`scope: global`, `dispatch: surface`) plus the
// pre-existing `peek.hover` (`K`, widened to global by this unit) and each
// surface's own `Enter`. `rungForKey` below is the keyboard half and is pure.

import { useCallback, useMemo } from "react";
import {
  appendTrail,
  codeUrl,
  symbolUrl,
  type PaneLoc,
  type TrailVia,
} from "../lib/codeUrl";
import { makeSnippet, type PaneId } from "../lib/navHistory";
import { currentTrailId, loadTrail, recordHop, visitCount } from "../lib/trail";
import { encode, type Location } from "./location";
import { useNavigator, type Navigator } from "./navigate";

/// The five rungs. `"here"` is the default commitment; `"peek"` is the
/// zero-commitment one.
export type RampRung = "peek" | "here" | "other" | "tab" | "window";

/// What a result row points at, plus whatever scent it happens to carry.
/// Every scent field is OPTIONAL and every one of them is rendered as an
/// honest "not known" when absent (`components/nav/ScentCard.tsx`) — a row
/// that knows nothing shows a card that says so, never an empty box.
export interface RampTarget {
  repo: string;
  path: string;
  line?: number;
  /// A `?sym=` address, when the row names a symbol rather than a line. Used
  /// for the new-tab/new-window rungs so the link survives code motion.
  sym?: string;
  ref?: string;
  /// The typed edge this row represents — what `via` the hop records.
  via: TrailVia;
  /// What the edge is ABOUT (`Order#total`, the search query, a finding
  /// slug). Rendered in the origin chip of a trail-linked tab.
  subject?: string;
  /// Scent: the symbol's own signature/kind/container, its trust class, the
  /// file's size, and a line of text to recognise the place by.
  signature?: string;
  kind?: string;
  container?: string | null;
  trust?: string;
  /// Bytes or lines, whichever the row's source knows; the card labels it.
  sizeLabel?: string;
  snippet?: string;
  /// Last session/commit that touched it, when the row carries one.
  lastTouched?: string;
}

/// Pure: which rung a keyboard event names, or `null`. Mirrors the registry
/// rows exactly (`ramp.open-tab` binds `Ctrl-Enter` and `o`;
/// `ramp.open-window` binds `O`; `peek.hover` binds `K`; `Enter` is each
/// surface's own row and `Shift-Enter` is `ramp.open-pane2`).
///
/// Deliberately reads a plain shape rather than a `KeyboardEvent`, so the
/// unit test can call it with a literal and a React synthetic event works
/// unchanged.
export function rungForKey(e: {
  key: string;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
  altKey?: boolean;
}): RampRung | null {
  if (e.altKey) return null;
  if (e.key === "Enter") {
    if (e.ctrlKey || e.metaKey) return "tab";
    if (e.shiftKey) return "other";
    return "here";
  }
  if (e.ctrlKey || e.metaKey) return null;
  if (e.key === "o") return "tab";
  if (e.key === "O") return "window";
  if (e.key === "K") return "peek";
  return null;
}

/// Pure: which rung a MOUSE event names. Middle-click and Ctrl/Cmd-click are
/// the browser's own "open elsewhere" gestures and are honoured as such, so a
/// row behaves like a link even where it is a `<div>` (the recon's table of
/// six surfaces where Cmd-click silently did nothing).
export function rungForMouse(e: {
  button: number;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
}): RampRung | null {
  if (e.button === 1) return "tab";
  if (e.button !== 0) return null;
  if (e.ctrlKey || e.metaKey) return "tab";
  if (e.shiftKey) return "other";
  return "here";
}

/// The URL a target resolves to, with no trail linkage. `sym` wins over
/// `line` when present — a symbol address survives code motion, and the line
/// rides along as the fallback anchor (`symbolUrl`'s own contract).
export function targetUrl(t: RampTarget): string {
  if (t.sym) {
    return symbolUrl(t.repo, t.sym, { fallbackPath: t.path, fallbackLine: t.line });
  }
  return codeUrl({ repo: t.repo, path: t.path, ref: t.ref, line: t.line });
}

export function targetPaneLoc(t: RampTarget): PaneLoc {
  return { path: t.path, ...(t.ref ? { ref: t.ref } : {}), ...(t.line ? { line: t.line } : {}) };
}

/// `path:line` — the label a trail step stores for its origin, and the one
/// the pane arrows preview.
export function locationLabel(loc: Location): string {
  if (!loc.path) return loc.page ?? loc.mode;
  return loc.anchor?.line ? `${loc.path}:${loc.anchor.line}` : loc.path;
}

export interface RampContext {
  /// The pane a plain `Enter` opens into.
  focusedPane: PaneId;
  /// The surface's own peek — `K` and a peek-rung activation call it. A
  /// surface with no peek of its own passes nothing, and `K` there does
  /// nothing rather than pretending (`CommandRoot` leaves an unhandled key
  /// alone by design).
  onPeek?: (t: RampTarget) => void;
  /// Called after a rung that navigated away in-app, so an overlay surface
  /// (the omnibox, a peek popup) can close itself.
  onNavigated?: (rung: RampRung) => void;
}

export interface Ramp {
  /// Run a rung against a target.
  activate(rung: RampRung, t: RampTarget): void;
  /// The href a row should carry so middle-click/Cmd-click work natively and
  /// the status bar shows a real destination (rows that are `<a>`/`<Link>`).
  hrefFor(t: RampTarget): string;
  /// How many times this trail has already landed on `t`'s file — the scent
  /// card's "visited N× in this trail". 0 when there is no trail yet.
  visits(t: RampTarget): number;
}

/// Build the shared handler. One hook, used by every surface in the Ramp
/// list; the surfaces differ only in what they put in a `RampTarget`.
export function useRamp(ctx: RampContext): Ramp {
  const nav = useNavigator(ctx.focusedPane);
  return useRampWith(nav, ctx);
}

/// The hook's body, with the navigator injected — so a unit test can drive
/// the whole Ramp with a fake navigator and no router.
export function useRampWith(nav: Navigator, ctx: RampContext): Ramp {
  const openElsewhere = useCallback(
    (t: RampTarget, rung: "tab" | "window") => {
      const from = nav.current();
      // Record the hop BEFORE opening, so the `?step=` we hand over always
      // indexes a step that exists (`lib/trail.ts`'s `recordHop` doc).
      const link = recordHop(t.repo, {
        from: encode(from),
        fromLabel: locationLabel(from),
        to: targetUrl(t),
        via: t.via,
        ...(t.subject ? { subject: t.subject } : {}),
        ...(t.trust ? { trust: t.trust } : {}),
      });
      const url = appendTrail(targetUrl(t), link);
      if (typeof window === "undefined") return;
      // `noopener` on the WINDOW rung: the cross-tab story is the trail in
      // the URL plus BroadcastChannel, never a `WindowProxy` handle (§P7).
      // The TAB rung omits it so the new tab inherits this tab's
      // `sessionStorage` copy of the trail — the fast path that makes the
      // origin chip render without waiting for a channel round trip.
      if (rung === "window") window.open(url, "_blank", "noopener");
      else window.open(url, "_blank");
    },
    [nav],
  );

  const activate = useCallback(
    (rung: RampRung, t: RampTarget) => {
      switch (rung) {
        case "peek":
          ctx.onPeek?.(t);
          return;
        case "here": {
          nav.go(targetUrl(t), t.via, {
            record: { pane: ctx.focusedPane, snippet: makeSnippet(t.snippet ?? "") },
          });
          ctx.onNavigated?.(rung);
          return;
        }
        case "other": {
          const from = nav.current();
          if (from.mode === "reader" && from.path) {
            // The other pane, through the existing `?pane2=` grammar — pane
            // location stays URL-derived (root CLAUDE.md #30).
            const other: Location = {
              ...from,
              sym: undefined,
              panes: { ...from.panes, pane2: targetPaneLoc(t) },
            };
            nav.go(other, t.via, {
              record: { pane: 2, snippet: makeSnippet(t.snippet ?? "") },
            });
          } else {
            // No reader underneath: there is no pane for this to be the OTHER
            // of, so it opens normally. Degrading loudly would mean refusing a
            // key the operator pressed; degrading silently to the same place
            // Enter would land is the honest read of "open it".
            nav.go(targetUrl(t), t.via, {
              record: { pane: ctx.focusedPane, snippet: makeSnippet(t.snippet ?? "") },
            });
          }
          ctx.onNavigated?.(rung);
          return;
        }
        case "tab":
        case "window":
          openElsewhere(t, rung);
          return;
      }
    },
    [ctx, nav, openElsewhere],
  );

  return useMemo<Ramp>(
    () => ({
      activate,
      hrefFor: (t) => targetUrl(t),
      visits: (t) => {
        const id = currentTrailId();
        const trail = id ? loadTrail(id) : null;
        const base = codeUrl({ repo: t.repo, path: t.path });
        return visitCount(trail, (url) => url.startsWith(base));
      },
    }),
    [activate],
  );
}
