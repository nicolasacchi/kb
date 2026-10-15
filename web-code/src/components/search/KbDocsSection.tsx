import { HEADER_ROW, type PaletteCursor } from "../../lib/paletteReducer";
import { kbDocsCaption, kbDocsRowCount, safeDocUrl } from "../../lib/kbDocs";
import type { KbDocHitOut, KbSearchOut } from "../../api/types";

export const KB_DOCS_LANE_LABEL = "Documents";

export interface KbDocsSectionProps {
  response: KbSearchOut | null;
  loading: boolean;
  /// This lane's index in the palette's section list (after every code lane).
  laneIndex: number;
  cursor: PaletteCursor;
  onHover: (row: number) => void;
}

function rowClassName(active: boolean): string {
  return `kbc-search__row${active ? " is-active" : ""}`;
}

function origin(hit: KbDocHitOut): string {
  const url = safeDocUrl(hit.url);
  if (!url) return hit.kb;
  try {
    const u = new URL(url);
    return `${hit.kb} · ${decodeURIComponent(u.pathname.replace(/^\/a\/[^/]+\//, ""))}`;
  } catch {
    return hit.kb;
  }
}

/// OL S1 - the omnibox "Documents" lane: kb's own search results, after the
/// code lanes. Markup reuses the lane classes so it reads as one more lane,
/// but it is NOT a `LaneSection` (those are the daemon's six, counted by the
/// e2e suite via `[data-kbc-lane]`); it carries `data-kbc-docs-lane` instead.
/// Rows are real new-tab anchors to the kb permalink; an `unavailable`
/// response renders one muted line, never an error.
export default function KbDocsSection({
  response,
  loading,
  laneIndex,
  cursor,
  onHover,
}: KbDocsSectionProps) {
  const rowCount = kbDocsRowCount(response);
  const headerActive = cursor.section === laneIndex && cursor.row === HEADER_ROW;
  const unavailable = response?.unavailable;

  return (
    <section
      className="kbc-search__section"
      data-kbc-docs-lane
      role="group"
      aria-label={KB_DOCS_LANE_LABEL}
    >
      <div
        className={`kbc-search__header${headerActive ? " is-active" : ""}`}
        data-kbc-role="header"
        onMouseEnter={() => onHover(HEADER_ROW)}
      >
        <span className="kbc-search__header-label">{KB_DOCS_LANE_LABEL}</span>
        <span className="kbc-search__header-count">
          {loading && !response ? "…" : unavailable ? "" : rowCount}
        </span>
      </div>
      <div className="kbc-search__rows">
        {unavailable && (
          <div className="kbc-search__note" data-kbc-docs-unavailable={unavailable}>
            {kbDocsCaption(unavailable)}
          </div>
        )}
        {!unavailable && !response && loading && (
          <div className="kbc-search__skeleton" aria-hidden="true">
            Searching…
          </div>
        )}
        {!unavailable && response && rowCount === 0 && (
          <div className="kbc-search__note">No matches</div>
        )}
        {!unavailable &&
          response &&
          response.hits.map((hit, i) => {
            const href = safeDocUrl(hit.url);
            const active = cursor.section === laneIndex && cursor.row === i;
            const body = (
              <>
                <span className="kbc-search__title">{hit.title || "(untitled)"}</span>
                <span className="kbc-search__meta">
                  {origin(hit)}
                  {hit.snippet ? ` · ${hit.snippet}` : ""}
                </span>
              </>
            );
            return href ? (
              <a
                key={`${hit.kb}:${hit.id}`}
                href={href}
                target="_blank"
                rel="noreferrer"
                className={rowClassName(active)}
                data-kbc-role="row"
                data-kbc-row={i}
                data-kbc-docs-row
                role="option"
                aria-selected={active}
                onMouseEnter={() => onHover(i)}
              >
                {body}
              </a>
            ) : (
              <div
                key={`${hit.kb}:${hit.id}`}
                className={rowClassName(active)}
                data-kbc-role="row"
                data-kbc-row={i}
              >
                {body}
              </div>
            );
          })}
      </div>
    </section>
  );
}
