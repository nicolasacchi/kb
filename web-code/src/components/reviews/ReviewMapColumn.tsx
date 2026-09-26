import { useMemo, type MutableRefObject } from "react";
import type { PseudoFile, ReviewFileRow } from "../../api/types";
import { Icon } from "../icons";
import { mapCensusText, type MapChapter, type MapRowState } from "../../lib/reviewMapColumn";
import { useSyntax } from "../../hooks/useSyntax";
import { ALL_FILES_CAP, useReviewAllFiles } from "../../hooks/useReviewAllFiles";
import { buildAllFilesTree, buildStatusSections } from "../../lib/reviewFileTree";
import ReviewFileTree, { type ReviewFileTreeHandle } from "./ReviewFileTree";

/// V80-M1 — a thread/finding count anchored on a path OUTSIDE the diff
/// (`files_changed` never named it), surfaced so it is discoverable even
/// in `Changed` mode — a thread is never hidden because its file has no
/// hunks.
export interface OutsideDiffFile {
  path: string;
  count: number;
}

/// V80-F1 — the "Changed (N) | All files" toggle, factored out of this
/// file so the full-page diff's mobile Files drawer (`routes/reviewDiff/
/// ReviewDiffRail.tsx`) can render it without a second copy of the
/// markup. V80-R4 (landed after this unit) moved the DESKTOP home of
/// this control out of the map column entirely, into
/// `ReviewDiffToolbar`'s View cluster (reachable with the map closed) —
/// that toolbar copy is its own, independent markup (`ReviewDiffToolbar.
/// tsx`, `.kbc-rdiff__files-mode`), not this component; this one now has
/// exactly ONE caller, the mobile drawer, where the toolbar's version is
/// unreachable (the drawer is a modal-ish overlay — its `.kbc-drawer-
/// scrim` covers the whole viewport and closes the drawer on any outside
/// click, so the toolbar sits behind it, not beside it). Pure
/// presentational either way: the mode itself lives in the URL
/// (`?files=`), threaded in from `ReviewDiff.tsx`.
export function FilesModeToggle({
  filesMode,
  fileCount,
  onSetFilesMode,
}: {
  filesMode: "changed" | "all";
  fileCount: number;
  onSetFilesMode: (mode: "changed" | "all") => void;
}) {
  return (
    <span className="kbc-rmap__files-mode" role="group" aria-label="File list">
      <button
        type="button"
        className={"kbc-rmap__files-mode-btn" + (filesMode === "changed" ? " is-active" : "")}
        aria-pressed={filesMode === "changed"}
        onClick={() => onSetFilesMode("changed")}
        title="List only files_changed"
        data-kbc-rdiff-files-mode="changed"
      >
        Changed ({fileCount})
      </button>
      <button
        type="button"
        className={"kbc-rmap__files-mode-btn" + (filesMode === "all" ? " is-active" : "")}
        aria-pressed={filesMode === "all"}
        onClick={() => onSetFilesMode("all")}
        title="List the tip sha's whole tree — unchanged files render plain"
        data-kbc-rdiff-files-mode="all"
      >
        All files
      </button>
    </span>
  );
}

/// V80-F1 — the "Outside the diff" chapter, factored out for the same
/// reason as `FilesModeToggle` above: the mobile Files drawer needs the
/// SAME surfaced-thread group the desktop map column already renders, not
/// a re-implementation. Renders nothing when `files` is empty (the caller
/// need not guard).
export function OutsideDiffChapter({
  files,
  currentPath,
  onPick,
}: {
  files: readonly OutsideDiffFile[];
  currentPath: string;
  onPick: (path: string) => void;
}) {
  if (files.length === 0) return null;
  return (
    <section className="kbc-rmap__chapter kbc-rmap__chapter--outside" data-kbc-rdiff-map-outside-diff>
      <h2 className="kbc-rmap__chapter-head">
        Outside the diff
        <span className="kbc-rmap__chapter-n">{files.length}</span>
      </h2>
      <ul className="kbc-rmap__list">
        {files.map((f) => (
          <li key={f.path}>
            <button
              type="button"
              className={"kbc-rmap__row" + (currentPath === f.path ? " is-current" : "")}
              title={`${f.path} — not in this patchset's diff, ${f.count} thread(s) anchored here`}
              onClick={() => onPick(f.path)}
              data-kbc-rdiff-map-outside={f.path}
            >
              <span className="kbc-rmap__path">{f.path}</span>
              <span className="kbc-rmap__chips">
                <span
                  className="kbc-rmap__chip kbc-rmap__chip--comments"
                  data-kbc-rdiff-map-outside-count={f.count}
                >
                  {f.count}
                </span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

export interface ReviewMapColumnProps {
  chapters: MapChapter[];
  /// Row state by path — built once by the route from data it already has
  /// (`ReviewFileRow`, the comments rollup, the findings join, the drafts
  /// tray, `classifyFile`). This component computes NOTHING about chips.
  stateByPath: ReadonlyMap<string, MapRowState>;
  /// The cursor file — the SAME `keys.cursor.fileIdx` the `]f`/`[f`
  /// motions drive, so the column and the keyboard can never disagree
  /// about which file is current.
  currentPath: string;
  fileCount: number;
  viewedCount: number;
  /// True when the chapters were derived from the reading order's own
  /// `reason` strings. Status sections replace those chapters as the
  /// grouping (V76-R2b); the flag is kept so the call site is unchanged.
  derived: boolean;
  onPick: (path: string) => void;
  onClose: () => void;
  /// V73-K2c (kbc-pseudo/1) — "chapter zero": the four review-scoped
  /// pseudo-files, listed BEFORE every real section.
  pseudoFiles: PseudoFile[];
  onPickPseudo: (name: string) => void;
  treeRef?: MutableRefObject<ReviewFileTreeHandle | null>;
  /// V80-M1 — "Changed (N) | All files" — the TOGGLE itself lives in
  /// `ReviewDiffToolbar`'s View cluster now (V80-R4: reachable with the
  /// map closed, since it also widens the jump palette and the "outside
  /// the diff" group). This component only reads `filesMode` to pick
  /// which tree it renders, and `repo`/`tipSha` to fetch the whole tree
  /// WHILE `filesMode === "all"` (`useReviewAllFiles`'s own `enabled`
  /// gate — the default view fires no extra request).
  repo: string;
  tipSha?: string;
  filesMode: "changed" | "all";
  /// V80-M1 — threads/findings anchored outside the diff, discoverable
  /// even in `Changed` mode. Omitted/empty renders nothing extra.
  outsideDiffFiles?: readonly OutsideDiffFile[];
}

/// The review diff's left FILE MAP column (V73-K2a, tree in V76-R2b).
/// It is a `<nav>`, not a new `<aside>` landmark: the region template in
/// `e2e/__snapshots__/regions.spec.ts/review-diff.aria.yml` is matched in
/// Playwright's CONTAIN mode, so the golden is untouched.
export default function ReviewMapColumn({
  chapters,
  stateByPath,
  currentPath,
  fileCount,
  viewedCount,
  derived: _derived,
  onPick,
  onClose,
  pseudoFiles,
  onPickPseudo,
  treeRef,
  repo,
  tipSha,
  filesMode,
  outsideDiffFiles,
}: ReviewMapColumnProps) {
  const syntaxQ = useSyntax();
  const files = useMemo(() => {
    const out: ReviewFileRow[] = [];
    const seen = new Set<string>();
    for (const ch of chapters) {
      for (const f of ch.files) {
        if (seen.has(f.path)) continue;
        seen.add(f.path);
        out.push(f);
      }
    }
    return out;
  }, [chapters]);
  const sections = useMemo(() => buildStatusSections(files), [files]);

  // V80-M1 — "All files" fetches the tip sha's whole tree ONLY while the
  // operator has switched to it (`useReviewAllFiles`'s own `enabled` gate);
  // `buildAllFilesTree` is likewise skipped in `changed` mode rather than
  // recomputed on every render for a tree nothing renders.
  const allFilesQ = useReviewAllFiles(repo, tipSha, filesMode === "all");
  const allFilesUnion = useMemo(
    () =>
      filesMode === "all" ? buildAllFilesTree(files, allFilesQ.data?.paths ?? []) : null,
    [filesMode, files, allFilesQ.data],
  );

  return (
    <nav className="kbc-rmap" aria-label="Review file map" data-kbc-rdiff-map>
      <header className="kbc-rmap__head">
        <span className="kbc-rmap__title">Files</span>
        <span className="kbc-rmap__census" data-kbc-rdiff-map-census>
          {mapCensusText(fileCount, viewedCount, sections.length, "section")}
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
      {/* V80-R4 (carry-over from M1) — `walkAllFiles` caps at
          `ALL_FILES_CAP` leaves and says so on the wire (`capped`); this
          was fetched but never SHOWN, which would have made a very large
          repo's "All files" list read as complete when it silently
          wasn't. An honest caption, never a silent truncation. */}
      {filesMode === "all" && allFilesQ.data?.capped && (
        <p className="kbc-rmap__note" data-kbc-rdiff-map-capped>
          first {ALL_FILES_CAP.toLocaleString()} files shown — narrow with the jump palette
        </p>
      )}
      <OutsideDiffChapter files={outsideDiffFiles ?? []} currentPath={currentPath} onPick={onPick} />
      {pseudoFiles.length > 0 && (
        <section className="kbc-rmap__chapter kbc-rmap__chapter--zero" data-kbc-rdiff-map-chapter-zero>
          <h2 className="kbc-rmap__chapter-head">
            The review itself
            <span className="kbc-rmap__chapter-n">{pseudoFiles.length}</span>
          </h2>
          <ul className="kbc-rmap__list">
            {pseudoFiles.map((f) => (
              <li key={f.name}>
                <button
                  type="button"
                  className={
                    "kbc-rmap__row kbc-rmap__row--pseudo" +
                    (currentPath === `~review/${f.name}` ? " is-current" : "")
                  }
                  disabled={!f.present}
                  title={f.present ? f.source : (f.reason ?? "not present")}
                  onClick={() => onPickPseudo(f.name)}
                  data-kbc-rdiff-map-pseudo={f.name}
                >
                  <span className="kbc-rmap__path">{f.path}</span>
                  {!f.present && (
                    <span className="kbc-rmap__chips" data-kbc-rdiff-map-pseudo-absent={f.name}>
                      {f.reason ?? "not present"}
                    </span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <ReviewFileTree
        files={files}
        stateByPath={stateByPath}
        currentPath={currentPath}
        syntaxRows={syntaxQ.data?.rows}
        onPick={onPick}
        rowAttr="map"
        treeRef={treeRef}
        mode={filesMode}
        allTree={allFilesUnion?.tree}
        changedPaths={allFilesUnion?.changedPaths}
      />
    </nav>
  );
}
