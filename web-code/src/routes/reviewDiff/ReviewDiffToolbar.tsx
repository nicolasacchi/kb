// `ReviewDiff`'s TOOLBAR — the page header strip (V73-K2b; the JSX moved out
// of `routes/ReviewDiff.tsx` verbatim, no behaviour change).
//
// Purely presentational for every DOMAIN value: every value and every
// callback still arrives as a prop, so this file derives no number and
// writes no URL param of its own — diff v2's rule is "the URL is the only
// state" (web-code/CLAUDE.md § Review diff v2), and a header that could
// hold a knob of its own is exactly how a parallel store starts.
//
// V80-R4 is the ONE exception, and it is deliberately NOT domain state:
// `openCluster` below tracks which of the three responsive overflow
// popovers (View / Navigate / Overlays) is open below the 1200px
// breakpoint. It is exactly the kind of ephemeral view chrome `NavMenu.tsx`
// already keeps locally (its own `open`/`idx`) — closing the tab, reloading,
// or widening the window all lose it with no observable effect on the
// review, which is the test for "doesn't belong in the URL."
import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
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
  /// V80-M1 — a jump hit outside `paths` (an "All files" candidate with no
  /// cursor index) navigates through THIS instead of `onGoFile` — the same
  /// path-based single-file-focus navigation the map column's tree uses.
  onPickFile: (path: string) => void;
  /// V80-R4 — "Changed (N) | All files" (V80-M1), moved here from the map
  /// column's own header so it is reachable with the map closed (it also
  /// widens the jump palette and the "outside the diff" group).
  filesMode: "changed" | "all";
  onSetFilesMode: (mode: "changed" | "all") => void;
}

/// V80-R4 — the three labelled clusters (`web-code/CLAUDE.md` § Review
/// diff v2's own doc on this unit). `Cluster` is a purely LAYOUT wrapper:
/// above the 1200px breakpoint its trigger button is CSS-hidden and the
/// body renders inline, byte-identical to the pre-cluster flat row; below
/// it, the trigger becomes the only visible affordance and the body
/// becomes a floating popover. Every wrapped control keeps its own
/// `data-kbc-*` attribute and stays mounted at all times — collapsing is a
/// CSS/visibility question, never a conditional-render one, so nothing a
/// keyboard test or a `data-kbc-*` selector depends on moves or disappears.
function Cluster({
  id,
  label,
  icon,
  open,
  onToggle,
  children,
}: {
  id: string;
  label: string;
  icon: React.ReactNode;
  open: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <div
      className={"kbc-rdiff__cluster" + (open ? " is-open" : "")}
      data-kbc-rdiff-cluster={id}
    >
      <button
        type="button"
        className="kbc-rdiff__cluster-trigger"
        aria-haspopup="true"
        aria-expanded={open}
        aria-label={label}
        onClick={onToggle}
        data-kbc-rdiff-cluster-trigger={id}
      >
        {icon}
        <Icon.ChevDown />
      </button>
      <div className="kbc-rdiff__cluster-body" role="group" aria-label={label}>
        {/* V80-R4 — the visible TEXT label: CSS-hidden at ≥1200px (the
            group's own `aria-label` above already names it for a screen
            reader there; the icon + the inter-cluster rule carry the
            SIGHTED grouping cue at that width, cheaply). Below 1200px,
            inside the popover, space is no longer the constraint and the
            text label is what tells the operator which cluster they
            opened. */}
        <span className="kbc-rdiff__cluster-label">{label}</span>
        {children}
      </div>
    </div>
  );
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
  onPickFile,
  filesMode,
  onSetFilesMode: setFilesMode,
}: ReviewDiffToolbarProps) {
  // V80-R4 — which overflow popover (if any) is open below 1200px (`reviews.css`'s own doc on the media query explains why 1200, not the brief's literal 1100). Only
  // ever ONE at a time — opening a second closes the first, the same
  // single-slot behaviour `NavMenu.tsx` uses for the Explore popover.
  const [openCluster, setOpenCluster] = useState<"view" | "navigate" | "overlays" | null>(null);
  const toolbarRef = useRef<HTMLElement | null>(null);

  useEffect(() => {
    if (!openCluster) return;
    function onDocClick(e: MouseEvent) {
      if (!toolbarRef.current?.contains(e.target as Node)) setOpenCluster(null);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpenCluster(null);
    }
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onKey);
    };
  }, [openCluster]);

  function toggle(cluster: "view" | "navigate" | "overlays") {
    setOpenCluster((cur) => (cur === cluster ? null : cluster));
  }

  return (
      <header className="kbc-rdiff__toolbar" data-kbc-rdiff-toolbar ref={toolbarRef}>
        <Link to={reviewUrl(repo, id)} className="kbc-rdiff__back" data-kbc-rdiff-back>
          <Icon.ArrowLeft />
          <span>Review</span>
        </Link>
        <h1 className="kbc-rdiff__title" data-kbc-rdiff-title title={title}>
          {title}
        </h1>

        {/* --- Navigate: where am I, and how do I move — ps · jump · prev/
            next · viewed meter · guided tour. --- */}
        <Cluster
          id="navigate"
          label="Navigate"
          icon={<Icon.Layers />}
          open={openCluster === "navigate"}
          onToggle={() => toggle("navigate")}
        >
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
            <span
              className="kbc-rdiff__ps-note"
              data-kbc-rdiff-ps-note
              title="interdiff — the interdiff wire carries no viewed or comment counts, so those columns are absent here, not zero"
            >
              interdiff — the interdiff wire carries no viewed or comment counts, so those
              columns are absent here, not zero
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
          {/* V80-R4 — icon-only (was "Prev"/"Next" text): part of the width
              budget that lets the Navigate cluster fit one line at 1280px
              (`--rdiff-toolbar-h`'s own comment). No test read this
              button's text — only its `data-kbc-*` + `disabled` — so the
              `aria-label`/`title` carry the word for anyone who needs it. */}
          <div className="kbc-rdiff__nav">
            <button
              type="button"
              className="kbc-review__action kbc-review__action--icon"
              disabled={!hasPrev}
              onClick={() => goFile(cursorIdx - 1)}
              aria-label="Previous file"
              title="Previous file"
              data-kbc-rdiff-prev-file
            >
              <Icon.Chevron className="kbc-rdiff__chevron--prev" />
            </button>
            <button
              type="button"
              className="kbc-review__action kbc-review__action--icon"
              disabled={!hasNext}
              onClick={() => goFile(cursorIdx + 1)}
              aria-label="Next file"
              title="Next file"
              data-kbc-rdiff-next-file
            >
              <Icon.Chevron />
            </button>
          </div>
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
                // V80-M1 — a hit outside `paths` (an "All files" candidate)
                // has no cursor index to go to; it opens single-file focus
                // instead, the same as picking it from the map/tree would.
                const idx = paths.indexOf(hit.path);
                if (idx >= 0) goFile(idx);
                else onPickFile(hit.path);
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
                        else onPickFile(item.path);
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
          {/* V80-R4 — "Guided tour"/"Exit tour" shortened to "Tour"/"Exit"
              (part of the same width budget as Prev/Next above); the full
              phrase moves to `title`, which is where it was already
              conditionally duplicated for the start button. */}
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
                  title="Exit guided tour"
                  data-kbc-rdiff-tour-exit
                >
                  Exit
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
                Tour
              </button>
            )}
          </div>
        </Cluster>

        {/* V80-R4 — a TOOLBAR-level sibling, not inside a `Cluster`: this
            is the MOBILE (<=860px) entry point for the file list
            (`mobile.css`'s own `display: none` above 860px / `inline-
            flex` at-or-below it), independent of the desktop cluster-
            collapse breakpoint (1200px) above. Nesting it inside
            `Cluster`'s `.kbc-rdiff__cluster-body` would have hidden it
            on mobile too — an ancestor `display: none` (the collapsed,
            unopened cluster body) hides every descendant regardless of
            that descendant's OWN `display` override
            (`mobile.spec.ts`'s "files toggle opens the drawer" caught
            exactly this). */}
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

        {/* --- View: how the diff itself renders — layout · files scope ·
            context · noise · map. --- */}
        <Cluster
          id="view"
          label="View"
          icon={<Icon.Eye />}
          open={openCluster === "view"}
          onToggle={() => toggle("view")}
        >
          {/* V80-R4 — icon-only (was "Unified"/"Split" text), matching the
              SAME toggle's existing icon-only rendering in
              `DiffFileHeader.tsx` (Commit/Compare's per-file header uses
              `Icon.UnifiedView`/`Icon.SplitView` for this exact concept
              already — this brings the review-diff toolbar's copy into
              line with it, and buys back width). `aria-label` unchanged. */}
          <div className="kbc-diff__modes" role="group" aria-label="Diff layout">
            <button
              type="button"
              className={"kbc-diff__mode" + (mode === "unified" ? " is-active" : "")}
              aria-pressed={mode === "unified"}
              aria-label="Unified diff"
              title="Unified diff"
              data-kbc-diff-mode-toggle="unified"
              onClick={() => setView("unified")}
            >
              <Icon.UnifiedView />
            </button>
            <button
              type="button"
              className={"kbc-diff__mode" + (mode === "split" ? " is-active" : "")}
              aria-pressed={mode === "split"}
              aria-label="Side-by-side diff"
              title="Side-by-side diff"
              data-kbc-diff-mode-toggle="split"
              onClick={() => setView("split")}
            >
              <Icon.SplitView />
            </button>
          </div>
          {/* V80-M1/R4 — "Changed | All files" (the per-mode COUNT dropped
              from the label here — the map column's own header still
              shows "Changed (N)" with the count when the map is open;
              this copy's job is picking the mode, and the bare word is
              part of the same width budget). */}
          <span className="kbc-rdiff__files-mode" role="group" aria-label="File list">
            <button
              type="button"
              className={"kbc-rdiff__files-mode-btn" + (filesMode === "changed" ? " is-active" : "")}
              aria-pressed={filesMode === "changed"}
              onClick={() => setFilesMode("changed")}
              title={`List only files_changed (${filesCount})`}
              data-kbc-rdiff-files-mode="changed"
            >
              Changed
            </button>
            <button
              type="button"
              className={"kbc-rdiff__files-mode-btn" + (filesMode === "all" ? " is-active" : "")}
              aria-pressed={filesMode === "all"}
              onClick={() => setFilesMode("all")}
              title="List the tip sha's whole tree — unchanged files render plain"
              data-kbc-rdiff-files-mode="all"
            >
              All
            </button>
          </span>
          {/* V73-K2a — the context dial, the noise toggle (with its census)
              and the map toggle. Every one of them writes the URL and
              reads nothing else. */}
          {/* V80-R4 — "ctx 3"/"ctx 10"/"whole file" shortened to "3"/"10"/
              "full": this select's own `aria-label`/`title` already name
              it as context lines, and a native `<select>` sizes to its
              WIDEST option regardless of which one is picked — "whole
              file" was the single widest option in this dial. */}
          <label className="kbc-rdiff__dial">
            <span className="kbc-sr-only">Context lines</span>
            <select
              value={String(ctxDial)}
              onChange={(e) => setCtx(parseDiffCtx(e.target.value))}
              aria-label="context lines"
              title="How much unchanged context each hunk shows. Widened rows are fetched from the file at this patchset, never synthesised."
              data-kbc-rdiff-ctx
            >
              <option value="3">3</option>
              <option value="10">10</option>
              <option value="full">full</option>
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
          <span
            className="kbc-rdiff__noise-census"
            data-kbc-rdiff-noise-census
            title={noiseCensusText(noiseStats)}
          >
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
        </Cluster>

        {/* --- Overlays: what's painted ON TOP of the diff — findings /
            comments / diagnostics / GitHub lane, drafts. --- */}
        <Cluster
          id="overlays"
          label="Overlays"
          icon={<Icon.Facts />}
          open={openCluster === "overlays"}
          onToggle={() => toggle("overlays")}
        >
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
            {/* V80-R4 — icon + number ("Findings 4 · Comments 4 · GitHub
                2" was the single widest string in the toolbar); the
                accessible name still spells the words out, only the
                VISIBLE text is compacted. No test read this element's
                text, only its presence via `data-kbc-rdiff-overlay-counts`. */}
            <span
              className="kbc-rdiff__overlay-counts"
              data-kbc-rdiff-overlay-counts
              aria-label={
                `Findings ${overlayCounts.findings} · Comments ${overlayCounts.comments}` +
                (githubThreadsData
                  ? ` · GitHub ${githubThreadCountTotal(githubThreadsData.threads)}`
                  : "")
              }
            >
              <span title="Findings">
                <Icon.Facts aria-hidden="true" /> {overlayCounts.findings}
              </span>
              <span title="Comments">
                <Icon.Comment aria-hidden="true" /> {overlayCounts.comments}
              </span>
              {githubThreadsData && (
                <span title="GitHub threads">
                  <Icon.PullRequest aria-hidden="true" /> {githubThreadCountTotal(githubThreadsData.threads)}
                </span>
              )}
            </span>
          </div>
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
        </Cluster>

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
