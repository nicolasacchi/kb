// `ReviewDiff`'s TOOLBAR — the page header strip (V73-K2b; the JSX moved out
// of `routes/ReviewDiff.tsx` verbatim, no behaviour change).
//
// Purely presentational: every value and every callback arrives as a prop, so
// this file holds no state and derives no number. That is the point of the
// split — diff v2's rule is "the URL is the only state" (web-code/CLAUDE.md
// § Review diff v2), and a header that could hold a knob of its own is
// exactly how a parallel store starts.
import { Link } from "react-router-dom";
import type { GithubThreadsOut, ReviewPatchset } from "../../api/types";
import { Icon } from "../../components/icons";
import PatchsetSwitcher from "../../components/reviews/PatchsetSwitcher";
import { githubThreadCountTotal } from "../../lib/githubThreads";
import { parseOverlayParam, type OverlayMode } from "../../lib/diffFindings";
import {
  parseDiffCtx,
  reviewUrl,
  type DiffCtxDial,
  type DiffPsSelection,
} from "../../lib/codeUrl";
import { noiseCensusText, type NoiseCensus, type NoiseMode } from "../../lib/diffNoise";
import type { DraftsState } from "../../lib/reviewDrafts";
import type { TourStop } from "../../lib/reviewTour";
import type { SpeedFilterHit } from "../../lib/speedSearch";
import { highlightSegments } from "../../lib/speedSearch";
import type { DiffMode } from "../../lib/prefs";
import type { ReviewFileRow } from "../../api/types";

export interface ReviewDiffToolbarProps {
  repo: string;
  id: number;
  title: string;
  patchsets: ReviewPatchset[];
  psSel: DiffPsSelection | null;
  psRange: { from: number; to: number } | null;
  psQuery: string;
  activePsNum: number | null | undefined;
  onSetPs: (next: DiffPsSelection | null) => void;
  viewedCount: number;
  filesCount: number;
  pct: number;
  mode: DiffMode;
  onSetView: (next: DiffMode) => void;
  overlay: OverlayMode;
  onSetOverlay: (next: OverlayMode) => void;
  overlayCounts: { findings: number; comments: number };
  githubThreadsData: GithubThreadsOut | null | undefined;
  ctxDial: DiffCtxDial;
  onSetCtx: (next: DiffCtxDial) => void;
  noiseMode: NoiseMode;
  onSetNoiseMode: (next: NoiseMode) => void;
  noiseStats: NoiseCensus;
  isMobile: boolean;
  mapOpen: boolean;
  mapParamOpen: boolean;
  onSetMapOpen: (next: boolean) => void;
  draftsOpen: boolean;
  onSetDraftsOpen: (next: boolean | ((v: boolean) => boolean)) => void;
  drafts: DraftsState;
  tourOn: boolean;
  tourStops: TourStop[];
  tourStepIdx: number;
  onStartTour: () => void;
  onExitTour: () => void;
  hasPrev: boolean;
  hasNext: boolean;
  cursorIdx: number;
  onGoFile: (idx: number) => void;
  filesOpen: boolean;
  onSetFilesOpen: (open: boolean) => void;
  jump: string;
  onSetJump: (next: string) => void;
  jumpHits: SpeedFilterHit<ReviewFileRow>[];
  paths: string[];
  onSetHelpOpen: (open: boolean) => void;
}

export default function ReviewDiffToolbar({
  repo,
  id,
  title,
  patchsets,
  psSel,
  psRange,
  psQuery,
  activePsNum,
  onSetPs: setPs,
  viewedCount,
  filesCount,
  pct,
  mode,
  onSetView: setView,
  overlay,
  onSetOverlay: setOverlay,
  overlayCounts,
  githubThreadsData,
  ctxDial,
  onSetCtx: setCtx,
  noiseMode,
  onSetNoiseMode: setNoiseMode,
  noiseStats,
  isMobile,
  mapOpen,
  mapParamOpen,
  onSetMapOpen: setMapOpen,
  draftsOpen,
  onSetDraftsOpen: setDraftsOpen,
  drafts,
  tourOn,
  tourStops,
  tourStepIdx,
  onStartTour: startTour,
  onExitTour: exitTour,
  hasPrev,
  hasNext,
  cursorIdx,
  onGoFile: goFile,
  filesOpen,
  onSetFilesOpen: setFilesOpen,
  jump,
  onSetJump: setJump,
  jumpHits,
  paths,
  onSetHelpOpen: setHelpOpen,
}: ReviewDiffToolbarProps) {
  return (
      <header className="kbc-rdiff__toolbar" data-kbc-rdiff-toolbar>
        <Link to={reviewUrl(repo, id)} className="kbc-rdiff__back" data-kbc-rdiff-back>
          <Icon.ArrowLeft />
          <span>Review</span>
        </Link>
        <h1 className="kbc-rdiff__title" data-kbc-rdiff-title>
          {title}
        </h1>
        <span className="kbc-review__ps-chip kbc-review__ps-chip--active" data-kbc-rdiff-ps>
          {psRange
            ? `ps${psRange.from}→ps${psRange.to}`
            : psSel === null && activePsNum != null
              ? `ps${activePsNum}`
              : `ps${psQuery}`}
        </span>
        {/* V73-K2a — the patchset switcher. `?ps=` was read but never
            written before this unit, so the full-page diff was pinned to
            latest. */}
        <PatchsetSwitcher
          patchsets={patchsets}
          value={psSel}
          latestPs={activePsNum ?? null}
          onChange={setPs}
        />
        {psRange && (
          <span className="kbc-rdiff__ps-note" data-kbc-rdiff-ps-note>
            interdiff — the interdiff wire carries no viewed or comment counts, so those columns are
            absent here, not zero
          </span>
        )}
        <div
          className="kbc-review__progress"
          title={`${viewedCount}/${filesCount} viewed`}
          data-kbc-review-progress={`${viewedCount}/${filesCount}`}
        >
          <div className="kbc-review__progress-bar" style={{ width: `${pct}%` }} />
          <span className="kbc-review__progress-label">
            {viewedCount}/{filesCount}
          </span>
        </div>
        <div className="kbc-diff__modes" role="group" aria-label="Diff layout">
          <button
            type="button"
            className={"kbc-diff__mode" + (mode === "unified" ? " is-active" : "")}
            aria-pressed={mode === "unified"}
            aria-label="Unified diff"
            data-kbc-diff-mode-toggle="unified"
            onClick={() => setView("unified")}
          >
            Unified
          </button>
          <button
            type="button"
            className={"kbc-diff__mode" + (mode === "split" ? " is-active" : "")}
            aria-pressed={mode === "split"}
            aria-label="Side-by-side diff"
            data-kbc-diff-mode-toggle="split"
            onClick={() => setView("split")}
          >
            Split
          </button>
        </div>
        <div className="kbc-rdiff__overlay" data-kbc-rdiff-overlay>
          <label className="kbc-rdiff__overlay-select">
            <span className="kbc-sr-only">Findings overlay</span>
            <select
              value={overlay}
              onChange={(e) => setOverlay(parseOverlayParam(e.target.value))}
              aria-label="findings overlay"
              data-kbc-rdiff-overlay-select
            >
              <option value="all">All</option>
              <option value="findings">Findings</option>
              <option value="comments">Comments</option>
              <option value="diagnostics">Diagnostics</option>
              <option value="github">GitHub</option>
              <option value="none">None</option>
            </select>
          </label>
          <span className="kbc-rdiff__overlay-counts" data-kbc-rdiff-overlay-counts>
            Findings {overlayCounts.findings} · Comments {overlayCounts.comments}
            {githubThreadsData ? ` · GitHub ${githubThreadCountTotal(githubThreadsData.threads)}` : ""}
          </span>
        </div>
        {/* V73-K2a — the context dial, the noise toggle (with its census),
            the map toggle and the drafts tray door. Every one of them
            writes the URL and reads nothing else. */}
        <div className="kbc-rdiff__dials" data-kbc-rdiff-dials>
          <label className="kbc-rdiff__dial">
            <span className="kbc-sr-only">Context lines</span>
            <select
              value={String(ctxDial)}
              onChange={(e) => setCtx(parseDiffCtx(e.target.value))}
              aria-label="context lines"
              title="How much unchanged context each hunk shows. Widened rows are fetched from the file at this patchset, never synthesised."
              data-kbc-rdiff-ctx
            >
              <option value="3">ctx 3</option>
              <option value="10">ctx 10</option>
              <option value="full">whole file</option>
            </select>
          </label>
          <button
            type="button"
            className={"kbc-review__action" + (noiseMode === "collapsed" ? " is-active" : "")}
            aria-pressed={noiseMode === "collapsed"}
            onClick={() => setNoiseMode(noiseMode === "collapsed" ? "shown" : "collapsed")}
            title={
              noiseMode === "collapsed"
                ? "Labelled hunks are collapsed — they are still counted and one click from open"
                : "Collapse hunks carrying a noise label (never hides one: each stays counted and expandable)"
            }
            data-kbc-rdiff-noise={noiseMode}
          >
            Noise
          </button>
          <span className="kbc-rdiff__noise-census" data-kbc-rdiff-noise-census>
            {noiseCensusText(noiseStats)}
          </span>
          {!isMobile && (
            <button
              type="button"
              className={"kbc-review__action" + (mapOpen ? " is-active" : "")}
              aria-pressed={mapOpen}
              onClick={() => setMapOpen(!mapParamOpen)}
              title="Show / hide the file map column (Space m)"
              data-kbc-rdiff-map-toggle={mapOpen ? "1" : "0"}
            >
              Map
            </button>
          )}
          <button
            type="button"
            className={"kbc-review__action" + (draftsOpen ? " is-active" : "")}
            aria-pressed={draftsOpen}
            onClick={() => setDraftsOpen((v) => !v)}
            title="Drafts you have composed but not published (Space w)"
            data-kbc-rdiff-drafts-toggle={drafts.drafts.length}
          >
            Drafts {drafts.drafts.length}
          </button>
        </div>
        <div className="kbc-rdiff__tour" data-kbc-rdiff-tour>
          {tourOn ? (
            <>
              <span className="kbc-rdiff__tour-progress" data-kbc-rdiff-tour-progress>
                Tour {tourStops.length === 0 ? 0 : tourStepIdx + 1}/{tourStops.length}
              </span>
              <button
                type="button"
                className="kbc-review__action"
                onClick={exitTour}
                data-kbc-rdiff-tour-exit
              >
                Exit tour
              </button>
            </>
          ) : (
            <button
              type="button"
              className="kbc-review__action"
              onClick={startTour}
              disabled={tourStops.length === 0}
              title={
                tourStops.length > 0
                  ? `Guided tour — ${tourStops.length} stop${tourStops.length === 1 ? "" : "s"}`
                  : "No files to tour"
              }
              data-kbc-rdiff-tour-start
            >
              Guided tour
            </button>
          )}
        </div>
        <div className="kbc-rdiff__nav">
          <button
            type="button"
            className="kbc-review__action"
            disabled={!hasPrev}
            onClick={() => goFile(cursorIdx - 1)}
            data-kbc-rdiff-prev-file
          >
            Prev
          </button>
          <button
            type="button"
            className="kbc-review__action"
            disabled={!hasNext}
            onClick={() => goFile(cursorIdx + 1)}
            data-kbc-rdiff-next-file
          >
            Next
          </button>
        </div>
        <button
          type="button"
          className="kbc-rdiff__files-toggle"
          onClick={() => setFilesOpen(true)}
          aria-label="jump to file"
          aria-expanded={filesOpen}
          data-kbc-rdiff-files-toggle
        >
          <Icon.List />
          Files
        </button>
        <div className="kbc-rdiff__jump">
          <input
            type="search"
            className="kbc-rdiff__jump-input"
            value={jump}
            onChange={(e) => setJump(e.target.value)}
            placeholder="Jump to file…"
            aria-label="jump to file"
            data-kbc-rdiff-jump
            onKeyDown={(e) => {
              if (e.key !== "Enter") return;
              const hit = jumpHits[0]?.item;
              if (!hit) return;
              e.preventDefault();
              const idx = paths.indexOf(hit.path);
              if (idx >= 0) goFile(idx);
              setJump("");
            }}
          />
          {jump.trim() !== "" && (
            <ul className="kbc-rdiff__jump-list" data-kbc-rdiff-jump-list>
              {jumpHits.slice(0, 12).map(({ item, ranges }) => (
                <li key={item.path}>
                  <button
                    type="button"
                    className="kbc-rdiff__jump-hit"
                    onClick={() => {
                      const idx = paths.indexOf(item.path);
                      if (idx >= 0) goFile(idx);
                      setJump("");
                    }}
                  >
                    {highlightSegments(item.path, ranges).map((seg, i) =>
                      seg.hit ? (
                        <mark key={i} className="kbc-speed-hit">
                          {seg.text}
                        </mark>
                      ) : (
                        <span key={i}>{seg.text}</span>
                      ),
                    )}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
        <button
          type="button"
          className="kbc-rdiff__help"
          onClick={() => setHelpOpen(true)}
          aria-label="keyboard shortcuts"
          data-kbc-rdiff-help
        >
          ?
        </button>
      </header>
  );
}
