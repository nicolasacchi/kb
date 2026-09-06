import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import type { AtlasEdge, DocSummary } from "../api/client";
import type { AtlasPointsResponse } from "../hooks/useAtlasPoints";
import AtlasView, { WorkingSetBar } from "./AtlasView";
import { artifactHref } from "../lib/artifactHref";
import { censusBump } from "../lib/census";
import { galleryUrl } from "../lib/galleryUrl";

// W3.M-d — the map-home shell: the atlas as a navigation surface, with a
// list projection of whatever is selected on it.
//
// THE GATE (read this before making it easier to reach). Map-home is an
// EVIDENCE-GATED promotion: the local-only atlas census (lib/census.ts)
// decides whether the map ever replaces the grid as the flagship home, and
// that census holds almost nothing yet — so the gate CANNOT have fired.
// Therefore this shell is reachable ONLY by an explicit `?shell=map` URL or
// by the Settings → Preferences "home" toggle an operator flips deliberately
// (`Prefs.home`, default `"grid"`). NAV_ITEMS is deliberately untouched: a
// prominent map entry point would manufacture the very evidence the census
// exists to measure. The numeric flip criterion is recorded in
// `api/prefs.ts` beside the `Home` type.
//
// Zero new fetches: the map's rows arrive from AtlasView itself
// (`onRowsChange` — the full-corpus `/atlas/points` merge the map is already
// drawing, else the gallery's loaded page), and the panel renders the
// selection out of those rows in hand. The pivot to a real gallery list goes
// through the SHIPPED `ids=` atom via `galleryUrl` (invariant #35 — no new
// filter atom), capped like every other atlas id pivot.
//
// Desktop only — the caller gates on `useIsMobile()`. A full-viewport map on
// a phone reads as a workbench, fights the ≤860px `.atlas` overrides, and
// contradicts the v0.23 one-button-one-sheet reader contract.

/// How many selected rows the panel renders before it says "and N more".
/// The panel is a ~340px column, not a virtualized grid; the gallery pivot
/// is the honest destination for a big selection.
const MAX_PANEL_ROWS = 200;

export default function MapShell({
  kb,
  docs,
  edges,
  points,
  docsTotal,
  onRecomputeDone,
}: {
  kb: string;
  /// The gallery's loaded page — AtlasView's fallback row source and its
  /// rich-field lookup table. Passed straight through.
  docs: DocSummary[];
  edges?: AtlasEdge[];
  points?: AtlasPointsResponse;
  docsTotal?: number;
  onRecomputeDone?: () => void;
}) {
  // The shell owns the lasso working set (AtlasView takes it as a
  // controlled prop) so the map and its list projection can never disagree.
  const [selection, setSelection] = useState<Set<string>>(() => new Set());
  // The rows the map is actually drawing, handed up by AtlasView.
  const [rows, setRows] = useState<DocSummary[]>(docs);
  const onRowsChange = useCallback((next: DocSummary[]) => setRows(next), []);

  // Census: one "opened" per mount of the shell (a deliberate `?shell=map`
  // entry), and one "selection made" per transition from empty → non-empty.
  // Density counters only — no streak, no goal, nothing rendered back at the
  // operator except the raw Settings table.
  useEffect(() => {
    censusBump("home.map.open");
  }, []);
  const hadSelection = useRef(false);
  useEffect(() => {
    const has = selection.size > 0;
    if (has && !hadSelection.current) censusBump("home.map.select");
    hadSelection.current = has;
  }, [selection]);

  // A kb switch invalidates every id in the working set (AtlasView clears its
  // own copy on `[kb]`; the controlled owner must do the same or the shell
  // would keep projecting kb A's ids over kb B's map).
  useEffect(() => {
    setSelection(new Set());
  }, [kb]);

  const selectedRows = useMemo(
    () => rows.filter((d) => selection.has(d.id)),
    [rows, selection],
  );

  return (
    <div className="map-shell" data-map-shell-selected={selection.size}>
      <div className="map-shell__map">
        <AtlasView
          docs={docs}
          kb={kb}
          edges={edges}
          points={points}
          docsTotal={docsTotal}
          onRecomputeDone={onRecomputeDone}
          shell
          selection={selection}
          onSelectionChange={setSelection}
          onRowsChange={onRowsChange}
        />
      </div>
      <aside
        className="map-shell__panel"
        aria-label="map selection"
        data-testid="map-shell-panel"
      >
        <header className="map-shell__head">
          <span className="map-shell__lab">
            {selection.size > 0 ? "selected" : "the map"}
          </span>
          <span className="map-shell__count">
            {selection.size > 0
              ? `${selection.size.toLocaleString()} of ${rows.length.toLocaleString()}`
              : `${rows.length.toLocaleString()} artifact${rows.length === 1 ? "" : "s"}`}
          </span>
        </header>
        {selection.size === 0 ? (
          <div className="map-shell__hint">
            <p>
              This is the map as a home. Nothing is selected yet — turn on{" "}
              <strong>lasso</strong> in the map toolbar and draw around a
              region to project it as a list here.
            </p>
            <p className="map-shell__hint-alt">
              Prefer a list to start with?{" "}
              <Link className="map-shell__link" to={galleryUrl(kb)}>
                the grid is still home
              </Link>
              .
            </p>
          </div>
        ) : (
          <>
            <ol className="map-shell__rows">
              {selectedRows.slice(0, MAX_PANEL_ROWS).map((d) => (
                <li key={d.id} className="map-shell__row">
                  <Link
                    className="map-shell__row-link"
                    to={artifactHref(kb, d.source_relative ?? d.path)}
                    title={d.source_relative ?? d.path}
                  >
                    <span className="map-shell__row-title">
                      {d.title || d.id}
                    </span>
                    {d.folder ? (
                      <span className="map-shell__row-meta">{d.folder}</span>
                    ) : null}
                  </Link>
                </li>
              ))}
            </ol>
            {selectedRows.length > MAX_PANEL_ROWS && (
              <p className="map-shell__more">
                and {(selectedRows.length - MAX_PANEL_ROWS).toLocaleString()}{" "}
                more — open the selection as a list to page through them
              </p>
            )}
            {/* Honesty: a lasso can enclose ids the panel has no row for
                (a dot drawn from the lean `/atlas/points` set that the
                loaded page never carried is still resolvable, but a stale
                id is not). Say so rather than quietly showing fewer. */}
            {selectedRows.length < selection.size && (
              <p className="map-shell__more">
                {(selection.size - selectedRows.length).toLocaleString()}{" "}
                selected dot
                {selection.size - selectedRows.length === 1 ? "" : "s"} aren't
                in the loaded row set and aren't listed here
              </p>
            )}
            {/* The working-set verbs (open as a gallery list · add to a
                list · new board · copy for an agent · clear) are the SAME
                component the atlas view docks under its canvas, hosted here
                instead so the shell never grows a second home for them —
                including the map→gallery pivot itself, which goes through
                `galleryUrl`'s SHIPPED `ids=` atom (#35), capped there at
                MAX_FILTER_IDS (500). `onPivot` only adds this shell's own
                gate counter to that one existing button. */}
            <WorkingSetBar
              kb={kb}
              selection={selection}
              docs={rows}
              onClear={() => setSelection(new Set())}
              onPivot={() => censusBump("home.map.pivot")}
            />
          </>
        )}
      </aside>
    </div>
  );
}
