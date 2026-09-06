import { useEffect, useMemo, useRef, useState } from "react";
import {
  getRecentFiles,
  getRecentLocations,
  type NavLocation,
  type RecentFile,
} from "../lib/navHistory";
import { filterLocations } from "../lib/navHistory";
import { highlightSegments } from "../lib/speedSearch";
import { TRAIL_VIA, VIA_LABEL } from "../lib/trail";
import { rungForKey, rungForMouse, type RampRung, type RampTarget } from "../nav/ramp";
import type { TrailVia } from "../lib/codeUrl";

export interface RecentLocationsProps {
  open: boolean;
  onClose: () => void;
  /// Jump to a location (host navigates the reader).
  onJump: (loc: { repo: string; path: string; line: number }) => void;
  /// Prefer showing locations for this repo first when the filter is empty
  /// (still shows the full ring; just a soft sort preference via sections).
  activeRepo?: string;
  /// V70-A6 — the Ramp (§P7): a Recent-Locations row is a result row, so
  /// `Shift-Enter`/`Ctrl-Enter`/`o`/`O` mean there what they mean everywhere.
  /// Absent ⇒ `Enter`-only, exactly as before.
  onRamp?: (rung: RampRung, target: RampTarget) => void;
}

type Row =
  | { kind: "header"; id: string; label: string }
  | { kind: "loc"; id: string; loc: NavLocation; ranges: import("../lib/speedSearch").MatchRange[] }
  | { kind: "file"; id: string; file: RecentFile; ranges: import("../lib/speedSearch").MatchRange[] };

/// V3.N1 — Recent Locations popup (`g.`). Modal list reusing the Omnibox's
/// backdrop/dialog chrome: type-ahead via the shared speed-search helper,
/// j/k or arrows + Enter to jump, Esc closes. Empty filter shows a
/// "Recent files" section (derived unique paths) above the full location
/// ring.
export default function RecentLocations({
  open,
  onClose,
  onJump,
  activeRepo,
  onRamp,
}: RecentLocationsProps) {
  const [q, setQ] = useState("");
  /// V70-A6 — filter by the typed edge ("definition hops only"), which the
  /// design names as the thing that REPLACES the draft's second stack: one
  /// store, one list, narrowed by the `via` it already records. `null` = all.
  const [viaFilter, setViaFilter] = useState<TrailVia | null>(null);
  const [cursor, setCursor] = useState(0);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // Snapshot on open so the list doesn't thrash under the operator's feet
  // if something else records a jump while the popup is up.
  const [locations, setLocations] = useState<NavLocation[]>([]);
  const [files, setFiles] = useState<RecentFile[]>([]);

  useEffect(() => {
    if (!open) return;
    setQ("");
    setViaFilter(null);
    setCursor(0);
    setLocations(getRecentLocations());
    setFiles(getRecentFiles());
    // Focus on next tick so the dialog is in the DOM.
    requestAnimationFrame(() => inputRef.current?.focus());
  }, [open]);

  /// The `via` kinds actually present in the ring — the chip row shows only
  /// what this operator's own history contains, never the full vocabulary
  /// (twelve dead chips would be noise, and would imply hops that never
  /// happened).
  const viasPresent = useMemo(() => {
    const seen = new Set<TrailVia>();
    for (const l of locations) if (l.via) seen.add(l.via);
    return TRAIL_VIA.filter((v) => seen.has(v));
  }, [locations]);

  const rows: Row[] = useMemo(() => {
    const filter = q.trim();
    const locs = viaFilter ? locations.filter((l) => l.via === viaFilter) : locations;
    if (filter === "") {
      const out: Row[] = [];
      if (files.length > 0 && !viaFilter) {
        out.push({ kind: "header", id: "hdr-files", label: "Recent files" });
        for (const f of files) {
          out.push({ kind: "file", id: `f:${f.repo}:${f.path}`, file: f, ranges: [] });
        }
      }
      if (locs.length > 0) {
        out.push({ kind: "header", id: "hdr-locs", label: "Recent locations" });
        for (const loc of locs) {
          out.push({ kind: "loc", id: `l:${loc.repo}:${loc.path}:${loc.line}:${loc.ts}`, loc, ranges: [] });
        }
      }
      return out;
    }
    // Filtered: locations only, ranked by the SHARED speed-search haystack
    // (`lib/navHistory.ts`'s `filterLocations`, which now includes the `via`
    // kind — so typing "usage" narrows to usage hops the same way typing a
    // path narrows to a file).
    const hits = filterLocations(locs, filter);
    // Soft preference: active-repo hits first within the same rank band —
    // we just stable-partition rather than rewrite the rank.
    const preferred = activeRepo
      ? [...hits.filter((h) => h.item.repo === activeRepo), ...hits.filter((h) => h.item.repo !== activeRepo)]
      : hits;
    return preferred.map((h) => ({
      kind: "loc" as const,
      id: `l:${h.item.repo}:${h.item.path}:${h.item.line}:${h.item.ts}`,
      loc: h.item,
      ranges: h.ranges,
    }));
  }, [q, locations, files, activeRepo, viaFilter]);

  const selectable = useMemo(() => rows.map((r, i) => (r.kind === "header" ? -1 : i)).filter((i) => i >= 0), [rows]);

  useEffect(() => {
    setCursor((c) => {
      if (selectable.length === 0) return 0;
      if (selectable.includes(c)) return c;
      return selectable[0];
    });
  }, [selectable]);

  /// A row as a Ramp target. `via` is the edge that ORIGINALLY produced the
  /// location — re-opening it is a `manual` hop, but the row still displays
  /// (and filters by) the edge it was first reached through, which is the
  /// fact the operator recognises it by.
  function rowTarget(row: Row): RampTarget {
    if (row.kind === "loc") {
      return {
        repo: row.loc.repo,
        path: row.loc.path,
        line: row.loc.line,
        via: row.loc.via ?? "manual",
        ...(row.loc.snippet ? { snippet: row.loc.snippet } : {}),
      };
    }
    if (row.kind === "file") {
      return { repo: row.file.repo, path: row.file.path, via: "manual" };
    }
    return { repo: activeRepo ?? "", path: "", via: "manual" };
  }

  function activate(row: Row) {
    if (row.kind === "loc") {
      onJump({ repo: row.loc.repo, path: row.loc.path, line: row.loc.line });
      onClose();
      return;
    }
    if (row.kind === "file") {
      onJump({ repo: row.file.repo, path: row.file.path, line: 1 });
      onClose();
    }
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onClose();
      return;
    }
    if (e.key === "ArrowDown" || e.key === "j") {
      // Only steal bare j when the input is empty — otherwise typing "j"
      // in the filter would be impossible.
      if (e.key === "j" && q !== "") return;
      e.preventDefault();
      if (selectable.length === 0) return;
      const idx = selectable.indexOf(cursor);
      const next = selectable[Math.min(selectable.length - 1, Math.max(0, idx) + 1)] ?? selectable[0];
      setCursor(next);
      return;
    }
    if (e.key === "ArrowUp" || e.key === "k") {
      if (e.key === "k" && q !== "") return;
      e.preventDefault();
      if (selectable.length === 0) return;
      const idx = selectable.indexOf(cursor);
      const prev = selectable[Math.max(0, (idx < 0 ? 0 : idx) - 1)] ?? selectable[0];
      setCursor(prev);
      return;
    }
    // V70-A6 — every Ramp rung, resolved by the ONE shared table. `Enter`
    // resolves to `"here"`, so the pre-A6 behaviour survives.
    const rung = rungForKey(e);
    if (!rung) return;
    const row = rows[cursor];
    if (!row || row.kind === "header") return;
    e.preventDefault();
    if (rung === "here" || !onRamp) {
      activate(row);
      return;
    }
    onRamp(rung, rowTarget(row));
    onClose();
  }

  if (!open) return null;

  return (
    <div
      className="kbc-omnibox-backdrop"
      role="presentation"
      data-kbc-recent-locations
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="kbc-omnibox kbc-recent-locs"
        role="dialog"
        aria-modal="true"
        aria-label="recent locations"
        onKeyDown={onKeyDown}
      >
        <div className="kbc-omnibox__head">
          <input
            ref={inputRef}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Filter recent locations…"
            className="kbc-omnibox__input"
            aria-label="filter recent locations"
          />
        </div>
        {viasPresent.length > 0 && (
          <div className="kbc-recent-locs__vias" data-kbc-recent-vias>
            <button
              type="button"
              className={"kbc-recent-locs__viachip" + (viaFilter === null ? " is-active" : "")}
              onClick={() => setViaFilter(null)}
            >
              all
            </button>
            {viasPresent.map((v) => (
              <button
                key={v}
                type="button"
                className={"kbc-recent-locs__viachip" + (viaFilter === v ? " is-active" : "")}
                data-kbc-recent-viachip={v}
                onClick={() => setViaFilter((cur) => (cur === v ? null : v))}
              >
                {VIA_LABEL[v]}
              </button>
            ))}
          </div>
        )}
        <div className="kbc-omnibox__body" role="listbox" aria-label="recent locations">
          {rows.length === 0 && (
            <div className="kbc-omnibox__hint">
              {locations.length === 0 ? "No locations recorded yet." : "No matches."}
            </div>
          )}
          {rows.map((row, i) => {
            if (row.kind === "header") {
              return (
                <div key={row.id} className="kbc-recent-locs__header">
                  {row.label}
                </div>
              );
            }
            const active = i === cursor;
            if (row.kind === "file") {
              return (
                <button
                  key={row.id}
                  type="button"
                  role="option"
                  aria-selected={active}
                  className={`kbc-recent-locs__row${active ? " is-active" : ""}`}
                  onMouseEnter={() => setCursor(i)}
                  onMouseDown={(e) => {
                    const rung = rungForMouse(e);
                    if (!rung || rung === "here" || !onRamp) return;
                    e.preventDefault();
                    onRamp(rung, rowTarget(row));
                  }}
                  onClick={() => activate(row)}
                >
                  <span className="kbc-recent-locs__path">
                    {row.file.repo !== activeRepo ? `${row.file.repo}/` : ""}
                    {row.file.path}
                  </span>
                </button>
              );
            }
            const pathLabel = `${row.loc.path}:${row.loc.line}`;
            // Ranges were computed against `path:line snippet` — only highlight
            // inside the path:line prefix when they fall within it.
            const pathRanges = row.ranges
              .filter((r) => r.start < pathLabel.length)
              .map((r) => ({ start: r.start, end: Math.min(r.end, pathLabel.length) }));
            return (
              <button
                key={row.id}
                type="button"
                role="option"
                aria-selected={active}
                className={`kbc-recent-locs__row${active ? " is-active" : ""}`}
                onMouseEnter={() => setCursor(i)}
                onMouseDown={(e) => {
                  const rung = rungForMouse(e);
                  if (!rung || rung === "here" || !onRamp) return;
                  e.preventDefault();
                  onRamp(rung, rowTarget(row));
                }}
                onClick={() => activate(row)}
              >
                <span className="kbc-recent-locs__path">
                  {row.loc.repo !== activeRepo ? `${row.loc.repo}/` : ""}
                  {pathRanges.length === 0
                    ? pathLabel
                    : highlightSegments(pathLabel, pathRanges).map((seg, si) =>
                        seg.hit ? (
                          <mark key={si} className="kbc-speedsearch__mark">
                            {seg.text}
                          </mark>
                        ) : (
                          <span key={si}>{seg.text}</span>
                        ),
                      )}
                </span>
                {/* V70-A6 — an entry renders as CODE (JetBrains' Recent
                    Locations shape, which the design cites): the cursor
                    line's own text, plus the typed edge that got you there.
                    An absent snippet says so rather than rendering an empty
                    row — the recon found five of six call sites passing
                    `snippet: ""`, which made this popup a bare path list. */}
                <code className="kbc-recent-locs__snippet" data-kbc-recent-snippet>
                  {row.loc.snippet || "— no line text recorded for this hop"}
                </code>
                {row.loc.via && (
                  <span className="kbc-recent-locs__via" data-kbc-recent-via={row.loc.via}>
                    {VIA_LABEL[row.loc.via]}
                  </span>
                )}
              </button>
            );
          })}
        </div>
        <div className="kbc-omnibox__foot">
          <span className="kbc-omnibox__keys">
            <kbd>↑↓</kbd>/<kbd>j k</kbd> move <kbd>Enter</kbd> jump <kbd>Esc</kbd> close
          </span>
        </div>
      </div>
    </div>
  );
}
