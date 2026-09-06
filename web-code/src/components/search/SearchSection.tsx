import { fromWire, highlightSegments, sliceRanges, type MatchRange } from "../../lib/matchRanges";
import type { ReactNode } from "react";
import { Link } from "react-router-dom";
import type {
  ChunkHit,
  FileHit,
  LaneSection,
  SessionHit,
  SymbolHit,
  TextFileResult,
  TranscriptHit,
} from "../../api/types";
import { readerUrl } from "../../lib/breadcrumbs";
import { HEADER_ROW, type PaletteCursor } from "../../lib/paletteReducer";
import { LANE_LABELS, fullSearchUrl, laneRowCount } from "../../lib/searchLanes";
import { flattenTextRows, formatSessionDate, highlightMatch } from "../../lib/searchRows";
import { resolveSearchTarget } from "../../lib/searchTargets";
import { rungForMouse, type RampRung, type RampTarget } from "../../nav/ramp";

export interface SearchSectionProps {
  section: LaneSection;
  /// This section's position in the CANONICAL lane order - must match
  /// `cursor.section`'s own indexing (both `lib/omniSearch.ts`'s
  /// `sectionsToRowCounts` and `lib/searchTargets.ts`'s
  /// `resolveSearchTarget` order via the same `orderSections`, so a caller
  /// that renders `orderSections(sections).map((s, i) => ...)` always lines
  /// up).
  laneIndex: number;
  /// V71-D2 — what the header reads. Defaults to the lane's own label; the
  /// results page passes `Lane · group` when the daemon grouped the page
  /// (`search::results::group_hits`). Presentation only — the lane's
  /// identity for keyboard/target purposes is still `section.lane`.
  headerLabel?: string;
  /// The FULL (unordered is fine - `resolveSearchTarget` re-orders)
  /// response section list, needed to resolve this section's own row
  /// targets via the shared resolver.
  sections: LaneSection[];
  cursor: PaletteCursor;
  query: string;
  repo?: string;
  /// The currently-expanded transcript row's `uuid` (the detail popover -
  /// see `lib/searchTargets.ts`'s `"popover"` target kind), or `null`/
  /// `undefined` when none is expanded.
  expandedTranscriptUuid?: string | null;
  /// Mouse-hover sets the cursor - `row === HEADER_ROW` for the header.
  onHover: (row: number) => void;
  /// Fired on a PLAIN (unmodified) click that results in a real navigation
  /// (a header or a "reader"-target row, both real `<Link>`s) - Omnibox
  /// uses this to close itself; the full `/search` page passes nothing
  /// (already there). A modifier-clicked row (ctrl/cmd/shift/middle) is
  /// left alone so the browser's native new-tab behavior applies.
  onNavigate?: () => void;
  /// Fired when a transcripts-lane row with no resolvable file target is
  /// clicked - toggles that row's detail popover.
  onPopover: (hit: TranscriptHit) => void;
  /// V70-A6 — the Ramp (§P7). Search rows were already real `<Link>`s, so
  /// Cmd-click and middle-click DID open a tab — but an untracked one, with
  /// no trail linkage and no origin chip. Routing them through the ONE
  /// shared handler is what makes "open elsewhere" carry the `via` edge and
  /// the way back. Absent ⇒ the browser's native link behaviour, unchanged.
  onRamp?: (rung: RampRung, target: RampTarget) => void;
}

function rowClassName(active: boolean): string {
  return `kbc-search__row${active ? " is-active" : ""}`;
}

/// Render `text` with the daemon's match indices marked. No ranges (an
/// older daemon, or a `recent` hit with no needle) renders the plain string
/// — never a guessed highlight.
function marked(text: string, ranges: MatchRange[]): ReactNode {
  if (ranges.length === 0) return text;
  return highlightSegments(text, ranges).map((seg, i) =>
    seg.hit ? (
      <mark key={i} className="kbc-search__mark">
        {seg.text}
      </mark>
    ) : (
      <span key={i}>{seg.text}</span>
    ),
  );
}

function basename(path: string): string {
  const parts = path.split("/");
  return parts[parts.length - 1] || path;
}

/// The reader/browser SPA - a lane header + its rows, shared by
/// `components/Omnibox.tsx` and `routes/Search.tsx` so both surfaces
/// render byte-identical lane markup and share exactly one target-
/// resolution codepath (`lib/searchTargets.ts`) for both mouse clicks and
/// (via the parent's keyboard handler) Enter.
export default function SearchSection({
  section,
  laneIndex,
  headerLabel,
  sections,
  cursor,
  query,
  repo,
  expandedTranscriptUuid,
  onHover,
  onNavigate,
  onPopover,
  onRamp,
}: SearchSectionProps) {
  const rowCount = laneRowCount(section);
  const headerActive = cursor.section === laneIndex && cursor.row === HEADER_ROW;

  function isRowActive(i: number): boolean {
    return cursor.section === laneIndex && cursor.row === i;
  }

  function handleNavClick(e: React.MouseEvent) {
    if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
    onNavigate?.();
  }

  /// A row whose target is `"reader"` - files/symbols/text/semantic, and
  /// (forward-compatibly) any future transcripts hit that carries a file.
  /// Falls back to an inert (non-navigating) row when the target can't be
  /// resolved (e.g. a text hit with no repo scope at all - see
  /// `resolveSearchTarget`'s doc).
  function readerRow(i: number, primary: ReactNode, secondary: ReactNode) {
    const target = resolveSearchTarget(sections, { section: laneIndex, row: i }, repo, query);
    const active = isRowActive(i);
    if (!target || target.kind !== "reader") {
      return (
        <div key={i} className={rowClassName(active)} data-kbc-role="row" data-kbc-row={i}>
          <span className="kbc-search__title">{primary}</span>
          <span className="kbc-search__meta">{secondary}</span>
        </div>
      );
    }
    const rampTarget: RampTarget = {
      repo: target.repo,
      path: target.path,
      line: target.line,
      via: "search",
      subject: query || undefined,
    };
    return (
      <Link
        key={i}
        to={readerUrl(target.repo, target.path, undefined, target.line)}
        className={rowClassName(active)}
        data-kbc-role="row"
        data-kbc-row={i}
        role="option"
        aria-selected={active}
        onMouseEnter={() => onHover(i)}
        onMouseDown={(e) => {
          const rung = rungForMouse(e);
          if (!rung || rung === "here" || !onRamp) return;
          e.preventDefault();
          onRamp(rung, rampTarget);
        }}
        onClick={handleNavClick}
      >
        <span className="kbc-search__title">{primary}</span>
        <span className="kbc-search__meta">{secondary}</span>
      </Link>
    );
  }

  /// A row whose target is `"external"` - sessions only. A real
  /// `target="_blank"` anchor (native new-tab, no `window.open` needed) -
  /// left open on click (the omnibox/search page never closes for an
  /// external open, mirroring a modifier-click's "stay put" behavior).
  function externalRow(i: number, primary: ReactNode, secondary: ReactNode) {
    const target = resolveSearchTarget(sections, { section: laneIndex, row: i }, repo, query);
    const active = isRowActive(i);
    if (!target || target.kind !== "external") return null;
    return (
      <a
        key={i}
        href={target.href}
        target="_blank"
        rel="noreferrer"
        className={rowClassName(active)}
        data-kbc-role="row"
        data-kbc-row={i}
        role="option"
        aria-selected={active}
        onMouseEnter={() => onHover(i)}
      >
        <span className="kbc-search__title">{primary}</span>
        <span className="kbc-search__meta">{secondary}</span>
      </a>
    );
  }

  function transcriptRow(hit: TranscriptHit, i: number) {
    const target = resolveSearchTarget(sections, { section: laneIndex, row: i }, repo, query);
    const meta = (
      <>
        <span className="kbc-search__chip">{hit.kind}</span> {hit.session_id.slice(0, 8)}
        {hit.tool_name ? ` · ${hit.tool_name}` : ""}
      </>
    );
    // Forward-compatible: if a future server field gives this hit a
    // resolvable file, it opens the reader like every other lane instead
    // of the popover (see `lib/searchLanes.ts`'s `transcriptReaderPath`).
    if (target?.kind === "reader") {
      return readerRow(i, hit.snippet, meta);
    }
    const active = isRowActive(i);
    const expanded = expandedTranscriptUuid === hit.uuid;
    return (
      <div key={i} className="kbc-search__transcript">
        <button
          type="button"
          className={rowClassName(active)}
          data-kbc-role="row"
          data-kbc-row={i}
          role="option"
          aria-selected={active}
          aria-expanded={expanded}
          onMouseEnter={() => onHover(i)}
          onClick={() => onPopover(hit)}
        >
          <span className="kbc-search__title">{hit.snippet}</span>
          <span className="kbc-search__meta">{meta}</span>
        </button>
        {expanded && (
          <div className="kbc-search__popover">
            <div className="kbc-search__popover-meta">
              {hit.project_dir} · session {hit.session_id}
            </div>
            <pre className="kbc-search__popover-snippet">{hit.snippet}</pre>
          </div>
        )}
      </div>
    );
  }

  function rows(): ReactNode {
    switch (section.lane) {
      case "files":
        return (section.results as FileHit[]).map((hit, i) => {
          // V71-D1 — the daemon's matcher ships its match indices, so the
          // path shows WHICH characters matched. Before kbcq/1 only the
          // client-side list filters highlighted; the server lanes returned
          // a bare score and the row rendered plain text.
          const ranges = fromWire(hit.ranges);
          return readerRow(
            i,
            basename(hit.path),
            <>
              {hit.repo} · {marked(hit.path, ranges)}
              {typeof hit.score === "number" ? ` · ${hit.score.toFixed(0)}` : ""}
            </>,
          );
        });
      case "symbols":
        return (section.results as SymbolHit[]).map((hit, i) => {
          // The server matched `Container::name` as ONE haystack, so the
          // name's own marks are the slice of those ranges that falls in
          // the name window — never a re-derivation on this side.
          const nameOffset = hit.container ? hit.container.length + 2 : 0;
          const ranges = sliceRanges(fromWire(hit.ranges), nameOffset, hit.name.length);
          return readerRow(
            i,
            <>
              {marked(hit.name, ranges)}
              {hit.container && <span className="kbc-search__container"> · {hit.container}</span>}
            </>,
            <>
              <span className="kbc-search__chip">{hit.kind}</span> {hit.repo} · {hit.path}:{hit.line_start}
            </>,
          );
        });
      case "text":
        return flattenTextRows(section.results as TextFileResult[]).map((row, i) => {
          const { before, match, after } = highlightMatch(row.line, row.byte_range);
          return readerRow(
            i,
            <>
              {before}
              <mark className="kbc-search__mark">{match}</mark>
              {after}
            </>,
            `${row.path}:${row.line_no}`,
          );
        });
      case "semantic":
        return (section.results as ChunkHit[]).map((hit, i) =>
          readerRow(
            i,
            hit.snippet,
            `${hit.repo} · ${hit.path}:${hit.span_start}-${hit.span_end} · ${hit.score.toFixed(2)}`,
          ),
        );
      case "sessions":
        return (section.results as SessionHit[]).map((hit, i) =>
          externalRow(i, hit.title || "(untitled session)", `${formatSessionDate(hit.started_at)} · kb digests`),
        );
      case "transcripts":
        return (section.results as TranscriptHit[]).map((hit, i) => transcriptRow(hit, i));
      default:
        return null;
    }
  }

  return (
    <section
      className="kbc-search__section"
      data-kbc-lane={section.lane}
      role="group"
      aria-label={headerLabel ?? LANE_LABELS[section.lane]}
    >
      <Link
        to={fullSearchUrl(query, repo)}
        className={`kbc-search__header${headerActive ? " is-active" : ""}`}
        data-kbc-role="header"
        role="option"
        aria-selected={headerActive}
        onMouseEnter={() => onHover(HEADER_ROW)}
        onClick={handleNavClick}
      >
        <span className="kbc-search__header-label">{headerLabel ?? LANE_LABELS[section.lane]}</span>
        <span className="kbc-search__header-count">
          {section.pending ? "…" : section.unavailable_reason ? "" : rowCount}
        </span>
      </Link>
      <div className="kbc-search__rows">
        {section.unavailable_reason && (
          <div className="kbc-search__note">{section.unavailable_reason}</div>
        )}
        {!section.unavailable_reason && section.pending && (
          <div className="kbc-search__skeleton" aria-hidden="true">
            Searching…
          </div>
        )}
        {!section.unavailable_reason && !section.pending && rowCount === 0 && (
          <div className="kbc-search__note">No matches</div>
        )}
        {!section.unavailable_reason && !section.pending && rows()}
      </div>
    </section>
  );
}
