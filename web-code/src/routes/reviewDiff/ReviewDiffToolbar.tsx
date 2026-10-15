// `ReviewDiff`'s TOOLBAR — the page header strip.
//
// Purely presentational for every DOMAIN value: every value and every
// callback arrives as a prop, so this file derives no number and writes no
// URL param of its own — diff v2's rule is "the URL is the only state"
// (web-code/CLAUDE.md § Review diff v2).
//
// OL C5 — the controls are ONE `ViewBar` (`@kb/ui/ViewBar`) of labelled
// clusters that reads left to right as "where am I (Patchset) -> move
// (Navigate) -> how it looks (View) -> my unsent work (Unsent)", with the
// two overlay controls (Lanes, Show map) in the bar's More menu. At most
// nine controls are visible on desktop. Controls with a bespoke shape
// (patchset pair, prev/next + progress, jump, tour, segments) are drawn by
// `renderControl` so every `data-kbc-*` hook sits where the specs find it.
import { Link } from "react-router";
import { ViewBar, type Cluster, type Control } from "@kb/ui/ViewBar";
import type { ReactNode } from "react";
import type { ReviewPatchset } from "../../api/types";
import { Icon } from "../../components/iconSet";
import PatchsetSwitcher from "../../components/reviews/PatchsetSwitcher";
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
import type { ReviewFileRow, SinceReport } from "../../api/types";
import { authorOnlyCaption } from "../../lib/reviewSince";

export interface ReviewDiffToolbarProps {
  repo: string;
  id: number;
  title: string;
  patchsets: ReviewPatchset[];
  psSel: DiffPsSelection | null;
  psRange: { from: number; to: number } | null;
  /// v0.44 F9b — the author delta for the `?ps=a..b` pair + its switch.
  since?: SinceReport;
  authorOnly: boolean;
  onSetAuthorOnly: (next: boolean) => void;
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
export default function ReviewDiffToolbar({
  repo,
  id,
  title,
  patchsets,
  psSel,
  psRange,
  since,
  authorOnly,
  onSetAuthorOnly,
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
  const noop = () => {};
  // Order is the visible-budget order: the first nine controls are drawn in
  // the bar, the rest (Lanes, Show map) land in More. Drafts sits BEFORE the
  // overlay cluster so unpublished human comments can never be the control
  // that overflows.
  const clusters: Cluster[] = [
    {
      label: "Patchset",
      controls: [{ id: "patchset", label: "Patchset", kind: "button", onSelect: noop }],
    },
    {
      label: "Navigate",
      controls: [
        { id: "files-step", label: "Files", kind: "button", onSelect: noop },
        { id: "jump", label: "Jump to file", kind: "button", onSelect: noop },
        { id: "tour", label: "Tour", kind: "button", onSelect: noop },
      ],
    },
    {
      label: "View",
      controls: [
        {
          id: "layout",
          label: "Layout",
          kind: "segment",
          value: mode,
          options: [
            { value: "unified", label: "Unified" },
            { value: "split", label: "Split" },
          ],
          onSelect: (v) => v && setView(v as DiffMode),
        },
        {
          id: "files-mode",
          label: "Files",
          kind: "segment",
          value: filesMode,
          options: [
            { value: "changed", label: "Changed" },
            { value: "all", label: "All" },
          ],
          onSelect: (v) => v && setFilesMode(v as "changed" | "all"),
        },
        {
          id: "context",
          label: "Context",
          kind: "select",
          value: String(ctxDial),
          options: [
            { value: "3", label: "3" },
            { value: "10", label: "10" },
            { value: "full", label: "full" },
          ],
          attrs: { "data-kbc-rdiff-ctx": "" },
          onSelect: (v) => setCtx(parseDiffCtx(v ?? "3")),
        },
        {
          id: "noise",
          label: "Hide noise",
          kind: "toggle",
          active: noiseMode === "collapsed",
          attrs: { "data-kbc-rdiff-noise": noiseMode },
          onSelect: () => setNoiseMode(noiseMode === "collapsed" ? "shown" : "collapsed"),
        },
      ],
    },
    {
      label: "Unsent",
      controls: [
        {
          id: "drafts",
          label: `Drafts ${drafts.drafts.length}`,
          kind: "toggle",
          active: draftsOpen,
          key: "Space w",
          attrs: { "data-kbc-rdiff-drafts-toggle": String(drafts.drafts.length) },
          onSelect: () => setDraftsOpen((v) => !v),
        },
      ],
    },
    {
      label: "Overlay",
      controls: [
        {
          id: "lanes",
          label: "Lanes",
          kind: "select",
          value: overlay,
          options: [
            { value: "all", label: "All" },
            { value: "findings", label: "Findings" },
            { value: "comments", label: "Comments" },
            { value: "diagnostics", label: "Diagnostics" },
            { value: "github", label: "GitHub" },
            { value: "none", label: "None" },
          ],
          attrs: { "data-kbc-rdiff-overlay-select": "" },
          onSelect: (v) => setOverlay(parseOverlayParam(v ?? "all")),
        },
        ...(isMobile
          ? []
          : ([
              {
                id: "map",
                label: "Show map",
                kind: "toggle",
                active: mapOpen,
                key: "Space m",
                attrs: { "data-kbc-rdiff-map-toggle": mapOpen ? "1" : "0" },
                onSelect: () => setMapOpen(!mapParamOpen),
              },
            ] satisfies Control[])),
      ],
    },
  ];

  function renderControl(c: Control, node: ReactNode): ReactNode {
    switch (c.id) {
      case "patchset":
        return (
          <div className="kbc-rdiff__ps">
            <span className="kbc-review__ps-chip kbc-review__ps-chip--active" data-kbc-rdiff-ps>
              {psRange
                ? `ps${psRange.from}→ps${psRange.to}`
                : psSel === null && activePsNum != null
                  ? `ps${activePsNum}`
                  : `ps${psQuery}`}
            </span>
            {/* The patchset switcher: base -> latest as one pair. */}
            <PatchsetSwitcher
              patchsets={patchsets}
              value={psSel}
              latestPs={activePsNum ?? null}
              onChange={setPs}
            />
            {psRange && since && (
              <label className="kbc-review__since-toggle" data-kbc-rdiff-author-only>
                <input
                  type="checkbox"
                  checked={authorOnly}
                  onChange={(e) => onSetAuthorOnly(e.target.checked)}
                />
                Author changes only
                <span className="kbc-review__since" data-kbc-rdiff-author-count>
                  {authorOnlyCaption(since)}
                </span>
              </label>
            )}
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
          </div>
        );
      case "files-step":
        return (
          <div className="kbc-rdiff__step">
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
          </div>
        );
      case "jump":
        return (
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
                // A hit outside `paths` (an "All files" candidate) has no
                // cursor index; it opens single-file focus instead.
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
        );
      case "tour":
        return (
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
        );
      case "layout":
        return (
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
        );
      case "files-mode":
        return (
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
        );
      case "noise":
        return (
          <>
            {node}
            <span
              className="kbc-rdiff__noise-census"
              data-kbc-rdiff-noise-census
              title={noiseCensusText(noiseStats)}
            >
              {noiseCensusText(noiseStats)}
            </span>
          </>
        );
      default:
        return node;
    }
  }

  return (
    <header className="kbc-rdiff__toolbar" data-kbc-rdiff-toolbar>
      <Link to={reviewUrl(repo, id)} className="kbc-rdiff__back" data-kbc-rdiff-back>
        <Icon.ArrowLeft />
        <span>Review</span>
      </Link>
      <h1 className="kbc-rdiff__title" data-kbc-rdiff-title title={title}>
        {title}
      </h1>

      {/* Mobile (<=860px) entry point for the file list; a toolbar-level
          sibling (outside the ViewBar) so it stays visible regardless of
          what the bar collapses. */}
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

      {/* On mobile the toolbar scrolls horizontally (mobile.css), which would
          clip a More menu, so every control stays in the bar there. */}
      <ViewBar
        clusters={clusters}
        maxVisible={isMobile ? 99 : 9}
        renderControl={renderControl}
      />

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
