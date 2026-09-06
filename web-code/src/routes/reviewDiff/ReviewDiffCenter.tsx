// `ReviewDiff`'s CENTER — the map column and the file stream (V73-K2b; the
// JSX moved out of `routes/ReviewDiff.tsx` verbatim, no behaviour change).
//
// Two layouts, one component, because they are ONE region: `single` renders
// the focused file plus its prev/next footer, `all` renders the lazy section
// per file. Everything it shows was computed by the route shell — the file
// order, the map row states, the per-file `DiffV2Api`. Re-deriving any of
// them here is what the kbc-tree/1 "renders rows it did NOT compute" rule
// forbids, in this route's shape.
import type { GithubThread, ReviewFileRow } from "../../api/types";
import ReviewMapColumn from "../../components/reviews/ReviewMapColumn";
import { reduceDiffKeys, type DiffKeysState } from "../../lib/diffKeys";
import type { MapChapter, MapRowState } from "../../lib/reviewMapColumn";
import type { OverlayMode } from "../../lib/diffFindings";
import type { DiffSide } from "../../lib/reviewComments";
import type { DiffMode } from "../../lib/prefs";
import { FileDiffBody, LazyDiffSection, type DiffV2Api } from "./DiffSections";
import { cssAttr } from "./helpers";

export interface ReviewDiffCenterProps {
  repo: string;
  id: number;
  psQuery: string;
  baseSha: string;
  tipSha: string;
  mode: DiffMode;
  single: boolean;
  focusPath: string;
  ordered: ReviewFileRow[];
  paths: string[];
  keys: DiffKeysState;
  setKeys: (fn: (s: DiffKeysState) => DiffKeysState) => void;
  hunkCounts: number[];
  cursorPath: string;
  cursorIdx: number;
  hasPrev: boolean;
  hasNext: boolean;
  mapOpen: boolean;
  chapters: MapChapter[];
  mapStateByPath: Map<string, MapRowState>;
  filesCount: number;
  viewedCount: number;
  stops: { path: string }[] | null;
  overlay: OverlayMode;
  flashThreadId: string | null;
  githubThreads: GithubThread[] | undefined;
  onHunks: (path: string, n: number) => void;
  onGoFile: (idx: number) => void;
  onSetMapOpen: (next: boolean) => void;
  onSetParam: (key: string, value: string | null) => void;
  onToggleViewed: (file: ReviewFileRow) => Promise<void>;
  composeFor: (path: string) => { side: DiffSide; line: number; token?: number } | null;
  v2For: (path: string) => DiffV2Api;
  withQuery: (href: string) => string;
  reviewDiffHref: (repo: string, id: number, file?: string) => string;
}

export default function ReviewDiffCenter({
  repo,
  id,
  psQuery,
  baseSha,
  tipSha,
  mode,
  single,
  focusPath,
  ordered,
  paths,
  keys,
  setKeys,
  hunkCounts,
  cursorPath,
  cursorIdx,
  hasPrev,
  hasNext,
  mapOpen,
  chapters,
  mapStateByPath,
  filesCount,
  viewedCount,
  stops,
  overlay,
  flashThreadId,
  githubThreads,
  onHunks: onHunks,
  onGoFile: goFile,
  onSetMapOpen: setMapOpen,
  onSetParam: setParam,
  onToggleViewed: toggleViewed,
  composeFor,
  v2For,
  withQuery,
  reviewDiffHref,
}: ReviewDiffCenterProps) {
  return (
      <div className={"kbc-rdiff__body" + (mapOpen ? " kbc-rdiff__body--mapped" : "")}>
        {mapOpen && (
          <ReviewMapColumn
            chapters={chapters}
            stateByPath={mapStateByPath}
            currentPath={cursorPath}
            fileCount={filesCount}
            viewedCount={viewedCount}
            derived={stops !== null && stops.length > 0}
            onPick={(path) => {
              const idx = paths.indexOf(path);
              if (idx >= 0) goFile(idx);
              document
                .querySelector(`[data-kbc-rdiff-file="${cssAttr(path)}"]`)
                ?.scrollIntoView({ block: "start" });
              setParam("file", path);
            }}
            onClose={() => setMapOpen(false)}
          />
        )}
        <div className="kbc-rdiff__stream">
        {ordered.length === 0 ? (
          <div className="kbc-reader__hint">No files in this patchset.</div>
        ) : single ? (
          <>
            {(() => {
              const file = ordered.find((f) => f.path === focusPath) ?? {
                path: focusPath,
                old_path: null,
                status: "M",
                additions: 0,
                deletions: 0,
                blob_sha: "",
                viewed: false,
                viewed_stale: false,
                open_annotations: 0,
              };
              const checked = !!(file.viewed && !file.viewed_stale);
              return (
                <section
                  className="kbc-rdiff__section kbc-rdiff__section--single"
                  data-kbc-rdiff-file={file.path}
                >
                  <header className="kbc-rdiff__section-head" data-kbc-rdiff-section={file.path}>
                    <span className="kbc-rdiff__section-path">{file.path}</span>
                    <span className="kbc-review__file-stats">
                      <span className="kbc-review__file-add">+{file.additions}</span>{" "}
                      <span className="kbc-review__file-del">−{file.deletions}</span>
                    </span>
                    {file.blob_sha && (
                      <label className="kbc-review__file-viewed">
                        <input
                          type="checkbox"
                          checked={checked}
                          onChange={() => void toggleViewed(file)}
                          aria-label={checked ? "mark unviewed" : "mark viewed"}
                          data-kbc-review-viewed={file.path}
                        />
                      </label>
                    )}
                  </header>
                  <FileDiffBody
                    repo={repo}
                    reviewId={id}
                    ps={psQuery}
                    path={file.path}
                    from={baseSha}
                    to={tipSha}
                    mode={mode}
                    onHunks={onHunks}
                    compose={composeFor(file.path)}
                    flashThreadId={flashThreadId}
                    overlay={overlay}
                    githubThreads={githubThreads}
                    v2={v2For(file.path)}
                  />
                </section>
              );
            })()}
            <footer className="kbc-rdiff__footer" data-kbc-rdiff-footer>
              <button
                type="button"
                className="kbc-review__action"
                disabled={!hasPrev}
                onClick={() => goFile(cursorIdx - 1)}
                data-kbc-rdiff-footer-prev
              >
                Prev file
              </button>
              <span className="kbc-rdiff__footer-pos">
                {cursorIdx + 1} / {paths.length}
              </span>
              <button
                type="button"
                className="kbc-review__action"
                disabled={!hasNext}
                onClick={() => goFile(cursorIdx + 1)}
                data-kbc-rdiff-footer-next
              >
                Next file
              </button>
            </footer>
          </>
        ) : (
          ordered.map((file) => (
            <LazyDiffSection
              key={file.path}
              repo={repo}
              reviewId={id}
              ps={psQuery}
              file={file}
              from={baseSha}
              to={tipSha}
              mode={mode}
              compose={composeFor(file.path)}
              flashThreadId={flashThreadId}
              overlay={overlay}
              githubThreads={githubThreads}
              collapsed={keys.collapsed.has(file.path)}
              current={file.path === cursorPath}
              checked={!!(file.viewed && !file.viewed_stale)}
              focusHref={withQuery(reviewDiffHref(repo, id, file.path))}
              onHunks={onHunks}
              onToggleViewed={(f) => void toggleViewed(f)}
              onToggleCollapse={() => {
                const idx = paths.indexOf(file.path);
                setKeys((s) => {
                  const at = idx >= 0 ? reduceDiffKeys(s, { type: "gotoFile", fileIdx: idx }, hunkCounts) : s;
                  return reduceDiffKeys(at, { type: "toggleCollapse" }, hunkCounts);
                });
              }}
              v2={v2For(file.path)}
            />
          ))
        )}
        </div>
      </div>
  );
}
