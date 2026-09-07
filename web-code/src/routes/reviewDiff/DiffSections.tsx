// `ReviewDiff`'s CENTER renderers — one file's diff body, and the lazy
// section wrapper the all-files stream mounts per file (V73-K2b; moved out of
// `routes/ReviewDiff.tsx` verbatim, no behaviour change).
//
// The route shell owns the page-wide facts (the queries, the cursor, the
// noise index, the drafts); these two components own ONE file each and
// receive everything else as props — including the shared `DiffV2Api` bag,
// which is built once at the route level precisely so a per-file component
// never re-derives a page-wide number.
import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import type { GithubThread, ReviewFileRow } from "../../api/types";
import DiffFile from "../../components/diff/DiffFile";
import { Icon } from "../../components/icons";
// S2-C — the shared Diagnostics inspector card (Reader mounts it via
// `InspectorRail`'s `diagnosticsCard` slot; here it's the diagnostics
// chip's on-demand expansion, `FileDiffBody`'s own doc).
import DiagnosticsCard from "../../components/provenance/DiagnosticsCard";
import type { HunkView } from "../../components/diff/HunkStrip";
import HunkTurnsPanel from "../../components/reviews/HunkTurnsPanel";
import { useDiagnostics } from "../../hooks/useDiagnostics";
import { useDiff } from "../../hooks/useDiff";
import { useFile } from "../../hooks/useFile";
import { useInViewOnce } from "../../hooks/useInViewOnce";
import { useReviewDiffComments } from "../../hooks/useReviewComments";
import { useReviewImpact } from "../../hooks/useReviews";
import { buildDiagnosticsView, diagnosticGutterMarks, diagnosticsChipText } from "../../lib/diagnostics";
import { parseUnifiedDiff, type ParsedDiff } from "../../lib/diff";
import { githubThreadVisibleInOverlay, type OverlayMode } from "../../lib/diffFindings";
import { githubOrphansForPath, indexGithubThreadsByLine } from "../../lib/githubThreads";
import { impactChipText, topChangedSymbol } from "../../lib/reviewImpact";
import { codeUrl, type DiffCtxDial } from "../../lib/codeUrl";
import {
  hunkHasThreads,
  hunkId,
  hunkStats,
  hunkNewSpan,
  type HunkThreadRef,
} from "../../lib/diffHunks";
import {
  classifyHunk,
  noiseCollapses,
  type MovedIndex,
  type NoiseLabel,
  type NoiseMode,
} from "../../lib/diffNoise";
import {
  combineExpand,
  contextCaption,
  dialNeedsFile,
  expandHunk,
  fileLines,
  type ExpandRequest,
} from "../../lib/diffContext";
import { draftsInSpan, type DraftsState } from "../../lib/reviewDrafts";
import type { DiffCommentsApi, DiffSide } from "../../lib/reviewComments";
import type { DiffMode } from "../../lib/prefs";

/// V73-K2a — everything diff v2 hands ONE file's body, in one bag rather
/// than a dozen props. Built once at the route level from data the page
/// already has; `FileDiffBody` derives this file's `HunkView[]` from it and
/// hands that to `DiffFile`, which renders it in either layout.
export interface DiffV2Api {
  ctx: DiffCtxDial;
  noiseMode: NoiseMode;
  /// `null` until at least one file's diff has parsed — a moved-block
  /// index over nothing would label nothing and say it had looked.
  movedIndex: MovedIndex | null;
  fileNoise: readonly NoiseLabel[];
  /// Server-backed per-hunk viewed ids (`review_hunk_viewed`).
  hunkViewed: ReadonlySet<string>;
  /// Operator folds, keyed by hunk id so a fold survives a re-render, a
  /// patchset switch that carries the hunk forward, and a layout toggle.
  folded: ReadonlySet<string>;
  /// Extra context clicked open per hunk, on top of the dial's own width.
  expand: ReadonlyMap<string, ExpandRequest>;
  drafts: DraftsState;
  /// The cursor's hunk index within THIS file, or `null` when the cursor
  /// is elsewhere.
  currentHunk: number | null;
  onParsed: (path: string, parsed: ParsedDiff) => void;
  onToggleHunkViewed: (path: string, id: string) => void;
  onToggleFold: (id: string) => void;
  onExpandHunk: (id: string, dir: "up" | "down") => void;
  /// Compose a DRAFT instead of posting (`lib/reviewDrafts.ts`). Replaces
  /// `DiffCommentsApi.onCreate` for the composer only — resolve/reply/
  /// delete/suggestion all still go straight to the server, because those
  /// act on threads that already landed.
  onDraftCreate: (
    path: string,
    side: DiffSide,
    line: number,
    lineEnd: number | undefined,
    body: string,
    intent: string,
  ) => Promise<void>;
  /// V73-K2c (kbc-hunk-turns/1) — the ONE currently-open hunk's turns panel,
  /// globally (a `kbc-hunkid/1` id already encodes its own path, so it only
  /// ever matches the ONE hunk it was computed from — `FileDiffBody` below
  /// checks membership against ITS OWN `hunkViews` before building the
  /// panel). `onToggleTurns` flips it open/closed; `null` means none open.
  turnsOpenId: string | null;
  onToggleTurns: (hunkId: string) => void;
}

export function FileDiffBody({
  repo,
  reviewId,
  ps,
  path,
  from,
  to,
  mode,
  onHunks,
  compose,
  flashThreadId,
  overlay,
  githubThreads,
  v2,
}: {
  repo: string;
  reviewId: number;
  ps: string;
  path: string;
  from: string;
  to: string;
  mode: DiffMode;
  onHunks: (path: string, n: number) => void;
  compose?: { side: DiffSide; line: number; token?: number } | null;
  flashThreadId?: string | null;
  overlay?: OverlayMode;
  /// PRR-F (design-addendum-2.md §A) — every GitHub thread on the review
  /// (fetched ONCE at the top level, `ReviewDiff`'s own doc), filtered/
  /// indexed to THIS path below, only while the overlay is in the
  /// `"github"` lane.
  githubThreads?: GithubThread[];
  v2?: DiffV2Api | null;
}) {
  const navigate = useNavigate();
  const { data, isLoading, error } = useDiff(repo, path, from, to);
  const parsed = useMemo(() => (data ? parseUnifiedDiff(data.diff) : null), [data]);
  const rawComments = useReviewDiffComments(repo, reviewId, ps, path, { compose, flashThreadId, overlay });
  // V73-K2a — the composer writes a DRAFT; every other callback is
  // untouched, so a landed thread still resolves/replies/deletes against
  // the server exactly as before.
  const onDraftCreate = v2?.onDraftCreate;
  const comments: DiffCommentsApi | null = useMemo(
    () =>
      rawComments && onDraftCreate
        ? {
            ...rawComments,
            onCreate: (side: DiffSide, line: number, lineEnd: number | undefined, body: string, intent: string) =>
              onDraftCreate(path, side, line, lineEnd, body, intent),
          }
        : rawComments,
    [rawComments, onDraftCreate, path],
  );
  useEffect(() => {
    if (parsed) onHunks(path, parsed.hunks.length);
  }, [parsed, path, onHunks]);

  // V73-K2a — publish the parsed diff upward ONCE per parse. The route
  // needs it for two page-wide derivations that a per-file component
  // structurally cannot do: the moved-block index (which is about OTHER
  // files) and the hunk-id → cursor mapping.
  const onParsed = v2?.onParsed;
  useEffect(() => {
    if (parsed && onParsed) onParsed(path, parsed);
  }, [parsed, path, onParsed]);

  // V73-K2a — the context dial. `3` needs nothing (the wire already sent
  // git's own -U3); `10`/`full` splice REAL rows out of the file at this
  // patchset's tip. `useFile` is gated on that, so the default page fires
  // no extra request per file.
  const wantFile = !!v2 && dialNeedsFile(v2.ctx);
  const fileQ = useFile(wantFile ? repo : undefined, wantFile ? path : undefined, to);
  const contentLines = useMemo(
    () =>
      fileQ.data && fileQ.data.encoding === "utf8" ? fileLines(fileQ.data.content) : null,
    [fileQ.data],
  );
  const ctxNote = v2
    ? contextCaption(v2.ctx, contentLines !== null, fileQ.isLoading)
    : null;

  const hunkViews: HunkView[] | null = useMemo(() => {
    if (!v2 || !parsed) return null;
    const threadRefs: HunkThreadRef[] = [];
    for (const list of rawComments?.byLine.values() ?? []) {
      for (const c of list) {
        if (c.path !== path) continue;
        threadRefs.push({
          side: c.side === "old" || c.side === "new" ? c.side : null,
          line: c.resolution?.line ?? null,
        });
      }
    }
    return parsed.hunks.map((hunk, i) => {
      const id = hunkId(path, hunk);
      const noise = classifyHunk(path, hunk, v2.fileNoise, v2.movedIndex);
      const byNoise = noiseCollapses(v2.noiseMode, noise);
      const folded = v2.folded.has(id);
      const expanded = expandHunk(hunk, contentLines, combineExpand(v2.ctx, v2.expand.get(id)));
      const span = hunkNewSpan(hunk);
      const stats = hunkStats(hunk);
      return {
        id,
        index: i,
        header: hunk.header,
        additions: stats.additions,
        deletions: stats.deletions,
        viewed: v2.hunkViewed.has(id),
        hasThreads: hunkHasThreads(hunk, threadRefs),
        draftCount: span
          ? draftsInSpan(v2.drafts, path, "new", span.start, span.end).length
          : 0,
        noise,
        lines: expanded.lines,
        addedBefore: expanded.addedBefore,
        addedAfter: expanded.addedAfter,
        moreAbove: expanded.moreAbove,
        moreBelow: expanded.moreBelow,
        collapsed: folded || byNoise,
        collapsedBy: folded ? ("fold" as const) : byNoise ? ("noise" as const) : null,
      };
    });
  }, [v2, parsed, path, contentLines, rawComments?.byLine]);

  // PRR-U9 (design-addendum-2.md §D) — diagnostics for this file. `useDiagnostics`
  // gates its own fetch on the repo's intel provider covering `path`'s
  // language, so this is a no-op query object (no request fired) for the
  // common case. `FileDiffBody` only ever mounts once this file's SECTION
  // has actually expanded (`LazyDiffSection`'s `useInViewOnce` gate, or the
  // always-expanded single-file focus view) — that's the "lazily fetched
  // when the file section first expands" contract, no extra plumbing here.
  const diagnostics = useDiagnostics(repo, path);
  const diagView = useMemo(
    () => buildDiagnosticsView(diagnostics.covered, diagnostics.data, diagnostics.isLoading),
    [diagnostics.covered, diagnostics.data, diagnostics.isLoading],
  );
  const diagChip = diagnosticsChipText(diagView);
  // Line marks are gated behind the overlay selector's own "diagnostics"
  // lane (design-addendum-2 §D) — computed only while `overlay ===
  // "diagnostics"`, `null` otherwise so `DiffFile`/the hunk renderers never
  // even see a stale map from a previous overlay mode.
  const diagByLine = useMemo(
    () => (overlay === "diagnostics" && diagView.rows ? diagnosticGutterMarks(diagView.rows) : null),
    [overlay, diagView.rows],
  );
  // S2-C — clicking the diagnostics chip toggles the SAME `DiagnosticsCard`
  // component the Reader's inspector rail mounts (`InspectorRail.tsx`'s
  // `diagnosticsCard` slot) open below this file's header, so the "Fixes"
  // quick-fixes affordance (`DiagnosticsCard`'s own doc) is available in
  // BOTH contexts off the ONE shared component — no second copy to keep in
  // sync. `useDiagnostics` above already fetched/cached this file's rows
  // (same TanStack Query key), so mounting the card here is a cache hit,
  // not a second network round trip.
  const [diagCardOpen, setDiagCardOpen] = useState(false);
  function onDiagJumpLine(line: number) {
    navigate(codeUrl({ repo, path, ref: to, line }));
  }

  // PRR-F (design-ui.md §12.2, "Reviewer X-ray") — same lazy-on-expand
  // contract as diagnostics above: `FileDiffBody` only mounts once this
  // file's section is in view, so this fetch is naturally deferred with no
  // extra plumbing. Independent of the overlay lane — the chip is a file-
  // header fact, not a thread/gutter overlay.
  const impactQ = useReviewImpact(repo, reviewId, path, true);
  const impactChip = impactChipText(impactQ.data);
  function onImpactClick() {
    const sym = topChangedSymbol(impactQ.data);
    if (!sym) return;
    navigate(codeUrl({ repo, path, ref: to, line: sym.line }));
  }

  // PRR-F (design-addendum-2.md §A) — GitHub-origin cards, own exclusive
  // overlay lane (`githubThreadVisibleInOverlay`'s own doc).
  const githubByLine = useMemo(
    () =>
      githubThreads && githubThreadVisibleInOverlay(overlay ?? "all")
        ? indexGithubThreadsByLine(githubThreads, path)
        : null,
    [githubThreads, overlay, path],
  );
  const githubOrphans = useMemo(
    () =>
      githubThreads && githubThreadVisibleInOverlay(overlay ?? "all")
        ? githubOrphansForPath(githubThreads, path)
        : [],
    [githubThreads, overlay, path],
  );

  // V73-K2c (kbc-hunk-turns/1) — the currently-open hunk's turns panel,
  // scoped to whether IT belongs to this file at all (`hunkId` already
  // factors in `path`, so it can only ever match one file's own
  // `hunkViews`). Mounting `<HunkTurnsPanel>` here IS the on-demand fetch
  // trigger — never built for a hunk nobody asked about.
  const turnsOpenHunkId =
    v2?.turnsOpenId && hunkViews?.some((hv) => hv.id === v2.turnsOpenId) ? v2.turnsOpenId : null;
  const turnsPanelNode = turnsOpenHunkId ? (
    <HunkTurnsPanel repo={repo} reviewId={reviewId} ps={ps} hunkId={turnsOpenHunkId} />
  ) : null;

  if (isLoading) return <div className="kbc-diff kbc-diff--loading">Loading diff…</div>;
  if (error) return <div className="kbc-diff kbc-diff--error">Failed to load diff</div>;
  if (!parsed) return null;
  return (
    <>
    {ctxNote && (
      <p className="kbc-rdiff__ctx-note" role="status" data-kbc-rdiff-ctx-note>
        {ctxNote}
      </p>
    )}
    <DiffFile
      path={path}
      parsed={parsed}
      mode={mode}
      comments={comments}
      hunkViews={hunkViews}
      currentHunk={v2?.currentHunk ?? null}
      onHunkFold={(hi) => {
        const view = hunkViews?.[hi];
        if (view) v2?.onToggleFold(view.id);
      }}
      onHunkViewed={(hi) => {
        const view = hunkViews?.[hi];
        if (view) v2?.onToggleHunkViewed(path, view.id);
      }}
      onHunkExpand={(hi, dir) => {
        const view = hunkViews?.[hi];
        if (view) v2?.onExpandHunk(view.id, dir);
      }}
      onHunkTurns={(hi) => {
        const view = hunkViews?.[hi];
        if (view) v2?.onToggleTurns(view.id);
      }}
      turnsOpenId={turnsOpenHunkId}
      turnsPanel={turnsPanelNode}
      diagnosticsChip={diagChip}
      diagnosticsByLine={diagByLine}
      onDiagnosticsClick={() => setDiagCardOpen((v) => !v)}
      diagnosticsCardOpen={diagCardOpen}
      diagnosticsCard={
        <DiagnosticsCard repo={repo} path={path} onJumpLine={onDiagJumpLine} reviewId={reviewId} />
      }
      impactChip={impactChip}
      onImpactClick={onImpactClick}
      githubByLine={githubByLine}
      githubOrphans={githubOrphans}
    />
    </>
  );
}

export function LazyDiffSection({
  repo,
  reviewId,
  ps,
  file,
  from,
  to,
  mode,
  collapsed,
  current,
  checked,
  focusHref,
  onHunks,
  onToggleViewed,
  onToggleCollapse,
  compose,
  flashThreadId,
  overlay,
  githubThreads,
  v2,
}: {
  repo: string;
  reviewId: number;
  ps: string;
  file: ReviewFileRow;
  from: string;
  to: string;
  mode: DiffMode;
  collapsed: boolean;
  current: boolean;
  checked: boolean;
  focusHref: string;
  onHunks: (path: string, n: number) => void;
  onToggleViewed: (file: ReviewFileRow) => void;
  onToggleCollapse: () => void;
  compose?: { side: DiffSide; line: number; token?: number } | null;
  flashThreadId?: string | null;
  overlay?: OverlayMode;
  githubThreads?: GithubThread[];
  v2?: DiffV2Api | null;
}) {
  const { ref, inView } = useInViewOnce();
  return (
    <section
      ref={ref}
      className={"kbc-rdiff__section" + (current ? " kbc-rdiff__section--current" : "")}
      data-kbc-rdiff-file={file.path}
    >
      <header className="kbc-rdiff__section-head" data-kbc-rdiff-section={file.path}>
        <button
          type="button"
          className="kbc-rdiff__collapse"
          onClick={onToggleCollapse}
          aria-expanded={!collapsed}
          title={collapsed ? "Expand" : "Collapse"}
          data-kbc-rdiff-collapse={file.path}
        >
          {collapsed ? <Icon.Expand /> : <Icon.Collapse />}
        </button>
        <span className="kbc-rdiff__section-path">{file.path}</span>
        <span className="kbc-review__file-stats">
          <span className="kbc-review__file-add">+{file.additions}</span>{" "}
          <span className="kbc-review__file-del">−{file.deletions}</span>
        </span>
        <label className="kbc-review__file-viewed" onClick={(e) => e.stopPropagation()}>
          <input
            type="checkbox"
            checked={checked}
            onChange={() => onToggleViewed(file)}
            aria-label={checked ? "mark unviewed" : "mark viewed"}
            data-kbc-review-viewed={file.path}
          />
        </label>
        <Link to={focusHref} className="kbc-rdiff__focus" data-kbc-rdiff-focus={file.path}>
          focus
        </Link>
      </header>
      {!collapsed && (
        <div className="kbc-rdiff__section-body">
          {inView ? (
            <FileDiffBody
              repo={repo}
              reviewId={reviewId}
              ps={ps}
              path={file.path}
              from={from}
              to={to}
              mode={mode}
              onHunks={onHunks}
              compose={compose}
              flashThreadId={flashThreadId}
              overlay={overlay}
              githubThreads={githubThreads}
              v2={v2}
            />
          ) : (
            <div className="kbc-rdiff__placeholder">Scroll to load diff</div>
          )}
        </div>
      )}
    </section>
  );
}
