import { useEffect, useRef } from "react";
import type { ReviewFileRow } from "../../api/types";
import { Icon } from "../icons";
import { mapCensusText, mapRowTitle, type MapChapter, type MapRowState } from "../../lib/reviewMapColumn";

export interface ReviewMapColumnProps {
  chapters: MapChapter[];
  /// Row state by path — built once by the route from data it already has
  /// (`ReviewFileRow`, the comments rollup, the findings join, the drafts
  /// tray, `classifyFile`). This component computes NOTHING.
  stateByPath: ReadonlyMap<string, MapRowState>;
  /// The cursor file — the SAME `keys.cursor.fileIdx` the `]f`/`[f`
  /// motions drive, so the column and the keyboard can never disagree
  /// about which file is current.
  currentPath: string;
  fileCount: number;
  viewedCount: number;
  /// True when the chapters were derived from the reading order's own
  /// `reason` strings rather than being a flat list — captioned, because
  /// a derived grouping presented as an authored one is a lie about the
  /// wire (authored chapters are a Track-K SHOULD, not built).
  derived: boolean;
  onPick: (path: string) => void;
  onClose: () => void;
}

function Chip({
  cls,
  label,
  title,
  attr,
}: {
  cls: string;
  label: string;
  title: string;
  attr?: Record<string, string>;
}) {
  return (
    <span className={`kbc-rmap__chip ${cls}`} title={title} {...attr}>
      {label}
    </span>
  );
}

/// The review diff's left FILE MAP column (V73-K2a). It is the desktop
/// promotion of the mobile "Files" drawer this page already had — the same
/// rows, the same cursor, one more home for neither: on mobile the drawer
/// stays the single entry point (root CLAUDE.md #30's one-mobile-entry
/// rule), and this column is simply not rendered there.
///
/// It is a `<nav>`, not a new `<aside>` landmark: the region template in
/// `e2e/__snapshots__/regions.spec.ts/review-diff.aria.yml` is matched in
/// Playwright's CONTAIN mode (banner/banner/banner), so the golden is
/// untouched by an added navigation region — and "jump to a file" is what
/// a nav IS.
export default function ReviewMapColumn({
  chapters,
  stateByPath,
  currentPath,
  fileCount,
  viewedCount,
  derived,
  onPick,
  onClose,
}: ReviewMapColumnProps) {
  const currentRef = useRef<HTMLButtonElement | null>(null);
  useEffect(() => {
    currentRef.current?.scrollIntoView({ block: "nearest" });
  }, [currentPath]);

  return (
    <nav className="kbc-rmap" aria-label="Review file map" data-kbc-rdiff-map>
      <header className="kbc-rmap__head">
        <span className="kbc-rmap__title">Files</span>
        <span className="kbc-rmap__census" data-kbc-rdiff-map-census>
          {mapCensusText(fileCount, viewedCount, chapters.length)}
        </span>
        <button
          type="button"
          className="kbc-rmap__close"
          onClick={onClose}
          aria-label="hide the file map"
          title="Hide the file map (Space m)"
          data-kbc-rdiff-map-close
        >
          <Icon.X />
        </button>
      </header>
      {derived && (
        <p className="kbc-rmap__note" data-kbc-rdiff-map-derived>
          chapters derived from the reading order&rsquo;s own reasons — the wire carries no authored
          chapters
        </p>
      )}
      {chapters.length === 0 && (
        <p className="kbc-rmap__note">No files in this patchset.</p>
      )}
      {chapters.map((ch, ci) => (
        <section className="kbc-rmap__chapter" key={ci} data-kbc-rdiff-map-chapter={ch.reason ?? ""}>
          <h2 className="kbc-rmap__chapter-head">
            {ch.reason ?? "not in the reading order"}
            <span className="kbc-rmap__chapter-n">{ch.files.length}</span>
          </h2>
          <ul className="kbc-rmap__list">
            {ch.files.map((f: ReviewFileRow) => {
              const st = stateByPath.get(f.path);
              const current = f.path === currentPath;
              return (
                <li key={f.path}>
                  <button
                    ref={current ? currentRef : undefined}
                    type="button"
                    className={"kbc-rmap__row" + (current ? " is-current" : "")}
                    title={st ? mapRowTitle(st) : f.path}
                    onClick={() => onPick(f.path)}
                    data-kbc-rdiff-map-row={f.path}
                    data-kbc-rdiff-map-current={current ? "1" : undefined}
                  >
                    <span className="kbc-rmap__path">{f.path}</span>
                    <span className="kbc-rmap__stats">
                      <span className="kbc-review__file-add">+{f.additions}</span>{" "}
                      <span className="kbc-review__file-del">−{f.deletions}</span>
                    </span>
                    <span className="kbc-rmap__chips">
                      {st?.viewed && (
                        <Chip
                          cls={
                            "kbc-rmap__chip--viewed" +
                            (st.viewedStale ? " kbc-rmap__chip--stale" : "")
                          }
                          label={st.viewedStale ? "viewed?" : "viewed"}
                          title={
                            st.viewedStale
                              ? "marked viewed, but this file's blob has changed since"
                              : "marked viewed at this blob"
                          }
                          attr={{ "data-kbc-rdiff-map-viewed": st.viewedStale ? "stale" : "1" }}
                        />
                      )}
                      {(st?.openComments ?? 0) > 0 && (
                        <Chip
                          cls="kbc-rmap__chip--comments"
                          label={`${st?.openComments}`}
                          title={`${st?.openComments} open comment thread(s)`}
                          attr={{ "data-kbc-rdiff-map-comments": String(st?.openComments) }}
                        />
                      )}
                      {(st?.findings ?? 0) > 0 && (
                        <Chip
                          cls="kbc-rmap__chip--findings"
                          label={`f${st?.findings}`}
                          title={`${st?.findings} finding(s) anchored in this file`}
                          attr={{ "data-kbc-rdiff-map-findings": String(st?.findings) }}
                        />
                      )}
                      {(st?.drafts ?? 0) > 0 && (
                        <Chip
                          cls="kbc-rmap__chip--draft"
                          label={`${st?.drafts} draft`}
                          title={`${st?.drafts} unpublished draft(s) on this file`}
                          attr={{ "data-kbc-rdiff-map-drafts": String(st?.drafts) }}
                        />
                      )}
                      {(st?.noise ?? []).map((n) => (
                        <Chip
                          key={n}
                          cls={`kbc-rmap__chip--noise kbc-noise--${n}`}
                          label={n}
                          title={`noise label: ${n} — the rule is on the hunk chip`}
                          attr={{ "data-kbc-rdiff-map-noise": n }}
                        />
                      ))}
                    </span>
                  </button>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </nav>
  );
}
