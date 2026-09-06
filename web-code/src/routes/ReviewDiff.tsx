import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams, useSearchParams } from "react-router-dom";
import type { GithubThread, ReviewDetailPr, ReviewFileRow, ReviewReadingStop } from "../api/types";
import DiffFile from "../components/diff/DiffFile";
import KeyboardHelp from "../components/KeyboardHelp";
import MobileDrawer from "../components/MobileDrawer";
import { Icon } from "../components/icons";
// S2-C — the shared Diagnostics inspector card (Reader mounts it via
// `InspectorRail`'s `diagnosticsCard` slot; here it's the diagnostics
// chip's on-demand expansion, `FileDiffBody`'s own doc).
import DiagnosticsCard from "../components/provenance/DiagnosticsCard";
import { useDiagnostics } from "../hooks/useDiagnostics";
import { useDiff } from "../hooks/useDiff";
import { useFile } from "../hooks/useFile";
import { useInViewOnce } from "../hooks/useInViewOnce";
import {
  useReviewComments,
  useReviewDiffComments,
  useReviewFindingDispositionMutations,
} from "../hooks/useReviewComments";
import {
  useDeleteReviewHunkViewed,
  useDeleteReviewViewed,
  useGithubThreads,
  usePutReviewHunkViewed,
  usePutReviewViewed,
  useReview,
  useReviewFiles,
  useReviewFindings,
  useReviewImpact,
  useReviewInterdiff,
  useReviewReadingOrder,
} from "../hooks/useReviews";
import { indexThreads, type DiffCommentsApi, type DiffSide } from "../lib/reviewComments";
import { buildDiagnosticsView, diagnosticGutterMarks, diagnosticsChipText } from "../lib/diagnostics";
import { parseUnifiedDiff, type ParsedDiff } from "../lib/diff";
import {
  buildThreadStops,
  nextUnviewedFileIdx,
  reduceDiffKeys,
  stepThreadStop,
  type DiffKeysAction,
  type DiffKeysState,
  initialDiffKeysState,
} from "../lib/diffKeys";
import {
  countByOverlay,
  dispositionLabel,
  findingsByAnnotationId,
  findingsBySlug,
  githubThreadVisibleInOverlay,
  nextOverlayMode,
  overlayParamValue,
  parseOverlayParam,
  type FindingDisposition,
  type OverlayMode,
} from "../lib/diffFindings";
import {
  githubOrphansForPath,
  githubThreadCountTotal,
  indexGithubThreadsByLine,
} from "../lib/githubThreads";
import { impactChipText, topChangedSymbol } from "../lib/reviewImpact";
import { buildTourStops, clampTourStep } from "../lib/reviewTour";
import {
  codeUrl,
  formatDiffPs,
  nextDiffCtx,
  parseDiffCtx,
  parseDiffMap,
  parseDiffPs,
  reviewDiffHref,
  reviewUrl,
  type DiffCtxDial,
  type DiffPsSelection,
} from "../lib/codeUrl";
// V73-K2a — diff v2's four pure halves: hunk identity, noise labels, the
// context dial, the drafts tray. Each is unit-pinned in its own file; this
// route only wires them together.
import {
  hunkHasThreads,
  hunkId,
  hunkStats,
  hunkNewSpan,
  type HunkThreadRef,
} from "../lib/diffHunks";
import {
  buildMovedIndex,
  classifyFile,
  classifyHunk,
  noiseCensus,
  noiseCensusText,
  noiseCollapses,
  parseNoiseMode,
  type MovedIndex,
  type NoiseLabel,
  type NoiseMode,
} from "../lib/diffNoise";
import {
  combineExpand,
  contextCaption,
  dialNeedsFile,
  expandHunk,
  fileLines,
  EXPAND_STEP,
  type ExpandRequest,
} from "../lib/diffContext";
import {
  clearDrafts,
  draftCountByPath,
  draftsInSpan,
  draftsToBatch,
  loadDrafts,
  newDraftId,
  removeDraft,
  saveDrafts,
  upsertDraft,
  EMPTY_DRAFTS,
  type DraftsState,
  type ReviewDraft,
} from "../lib/reviewDrafts";
import { mapChapters, type MapRowState } from "../lib/reviewMapColumn";
import { postAnnotationsBatch } from "../api/client";
import type { HunkView } from "../components/diff/HunkStrip";
import DraftsTray from "../components/reviews/DraftsTray";
import PatchsetSwitcher from "../components/reviews/PatchsetSwitcher";
import ReviewMapColumn from "../components/reviews/ReviewMapColumn";
import { useConfirm } from "../components/ConfirmProvider";
import { useIsMobile } from "../hooks/useIsMobile";
import { useQueryClient } from "@tanstack/react-query";
import type { DiffMode } from "../lib/prefs";
import { loadDiffMode, saveDiffMode } from "../lib/prefs";
import { highlightSegments, speedFilterItems } from "../lib/speedSearch";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { toast } from "../lib/toast";
import "../styles/reviews.css";

/// `/r/:repo/~reviews/:id/diff` and `/r/:repo/~reviews/:id/diff/*`.
/// Query: `ps` `view` `line` `side` `file` `thread` `finding` `overlay`
/// (PRR-U3). `?thread=`/`?finding=` are parsed and kept; scrolling a thread
/// is V4.C4, extended by PRR-U3 to also drive `t`/`T` stepping through the
/// SAME flash machinery. When `line`+`side` are also present this page
/// falls back to the line flash.

// V70-A5 — the private 500 ms chord machine that used to live here is GONE.
// It was one of three chord machines with three different timeout policies
// (this one 500 ms, the CM6 buffer's never, kb's own SPA 800 ms).
// `commands/CommandRoot.tsx` now owns the single pending-chord state for the
// whole app on vim's policy — a prefix never expires by itself, Escape
// cancels — and this route owns HANDLERS, not keys.

function parseView(raw: string | null): DiffMode | null {
  return raw === "unified" || raw === "split" ? raw : null;
}

function parseSide(raw: string | null): "old" | "new" | null {
  return raw === "old" || raw === "new" ? raw : null;
}

function parseLine(raw: string | null): number | null {
  if (raw == null || raw === "") return null;
  const n = Number(raw);
  return Number.isFinite(n) && n > 0 ? n : null;
}

function cssAttr(value: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") return CSS.escape(value);
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

function orderedRows(files: ReviewFileRow[], stops: ReviewReadingStop[] | null): ReviewFileRow[] {
  if (!stops || stops.length === 0) return files;
  const byPath = new Map(files.map((f) => [f.path, f]));
  const seen = new Set<string>();
  const out: ReviewFileRow[] = [];
  for (const s of stops) {
    const f = byPath.get(s.path);
    if (f && !seen.has(f.path)) {
      out.push(f);
      seen.add(f.path);
    }
  }
  for (const f of files) {
    if (!seen.has(f.path)) out.push(f);
  }
  return out;
}

function msg(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

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
}

function FileDiffBody({
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

function LazyDiffSection({
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

export default function ReviewDiff() {
  const { repo = "", id: idParam = "" } = useParams<{ repo: string; id: string }>();
  const splat = useParams()["*"] ?? "";
  const focusPath = splat
    ? splat
        .split("/")
        .map((s) => {
          try {
            return decodeURIComponent(s);
          } catch {
            return s;
          }
        })
        .filter((s) => s !== "")
        .join("/")
    : "";
  const single = focusPath.length > 0;
  const id = Number(idParam);
  const idOk = Number.isFinite(id) && id > 0;
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();

  // V73-K2a — `?ps=` is now WRITTEN as well as read (the full-page diff was
  // permanently pinned to "latest" before this unit). A bare number picks
  // one patchset; `a..b` picks the INTERDIFF between two, which is a
  // different route with a different, thinner file row — see `filesRows`
  // below for the honest degrade that entails.
  const psSel = parseDiffPs(searchParams.get("ps"));
  const psRange = psSel !== null && typeof psSel !== "number" ? psSel : null;
  const psQuery = psSel === null ? "latest" : String(typeof psSel === "number" ? psSel : psSel.to);
  const ctxDial = parseDiffCtx(searchParams.get("ctx"));
  const noiseMode: NoiseMode = parseNoiseMode(searchParams.get("noise"));
  const mapParamOpen = parseDiffMap(searchParams.get("map"));
  const hunkParam = searchParams.get("hunk");
  const urlView = parseView(searchParams.get("view"));
  const line = parseLine(searchParams.get("line"));
  const side = parseSide(searchParams.get("side"));
  const fileHint = searchParams.get("file") ?? "";
  const threadId = searchParams.get("thread");
  // PRR-U3 — `?finding=<f-slug>` deep link + `?overlay=` toggle (design-ui.md
  // §5/§S3).
  const findingParam = searchParams.get("finding");
  const overlay: OverlayMode = parseOverlayParam(searchParams.get("overlay"));

  const [prefMode, setPrefMode] = useState<DiffMode>(() => loadDiffMode());
  const mode: DiffMode = urlView ?? prefMode;

  const reviewQ = useReview(repo, idOk ? id : undefined);
  const filesQ = useReviewFiles(repo, idOk ? id : undefined, psQuery);
  const orderQ = useReviewReadingOrder(repo, idOk ? id : undefined, true);
  const commentsQ = useReviewComments(repo, idOk ? id : undefined, psQuery, true);
  const findingsQ = useReviewFindings(repo, idOk ? id : undefined, { ps: psQuery });
  const dispositionMut = useReviewFindingDispositionMutations(repo, idOk ? id : 0);
  const putViewed = usePutReviewViewed(repo, id);
  const delViewed = useDeleteReviewViewed(repo, id);
  const putHunkViewed = usePutReviewHunkViewed(repo, id);
  const delHunkViewed = useDeleteReviewHunkViewed(repo, id);
  const confirm = useConfirm();
  const isMobile = useIsMobile();
  const queryClient = useQueryClient();
  // V73-K2a — the interdiff arm of the patchset switcher. Only fires for a
  // `?ps=a..b` range; `useReviewInterdiff`'s own `enabled` gate keeps the
  // ordinary single-patchset page at exactly the request count it had.
  const interdiffQ = useReviewInterdiff(repo, idOk ? id : undefined, psRange?.from, psRange?.to, psRange !== null);
  // PRR-F (design-addendum-2.md §A) — fetched ONCE here (the review's
  // `pr_number`, once known), then threaded down to every `FileDiffBody` —
  // one request for the whole page, not one per file.
  const reviewPrNumber = (reviewQ.data as ReviewDetailPr | undefined)?.pr_number;
  const reviewGithubThreadsQ = useGithubThreads(repo, idOk ? id : undefined, reviewPrNumber);

  // PRR-U3 — findings joined by annotation id / slug (see
  // `lib/diffFindings.ts`'s module doc for the join rationale).
  const findingsById = useMemo(
    () => findingsByAnnotationId(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );
  const findingsSlugMap = useMemo(
    () => findingsBySlug(findingsQ.data?.findings ?? []),
    [findingsQ.data],
  );
  const allThreadIds = useMemo(() => {
    const ids: string[] = [];
    for (const g of commentsQ.data?.groups ?? []) for (const c of g.comments) ids.push(c.id);
    return ids;
  }, [commentsQ.data]);
  const overlayCounts = useMemo(
    () => countByOverlay(allThreadIds, findingsById),
    [allThreadIds, findingsById],
  );
  const threadsByPath = useMemo(() => {
    const m = new Map<string, { id: string }[]>();
    for (const g of commentsQ.data?.groups ?? []) m.set(g.path, g.comments.map((c) => ({ id: c.id })));
    return m;
  }, [commentsQ.data]);
  const findingSeverityByThreadId = useMemo(() => {
    const m = new Map<string, string>();
    for (const f of findingsQ.data?.findings ?? []) m.set(f.annotation_id, f.severity);
    return m;
  }, [findingsQ.data]);
  const [composeReq, setComposeReq] = useState<{
    path: string;
    side: DiffSide;
    line: number;
    token: number;
  } | null>(null);

  // V73-K2a — in RANGE mode the rows come from `GET .../interdiff`, whose
  // `ReviewInterdiffFile` carries no viewed/annotation fields at all. They
  // are filled with honest absence (`viewed:false`, `blob_sha:""`,
  // `open_annotations:0`) and the toolbar captions the mode, rather than
  // rendering a zeroed viewed column that would read as "nothing viewed."
  const files: ReviewFileRow[] = useMemo(() => {
    if (psRange) {
      return (interdiffQ.data?.files ?? []).map((f) => ({
        path: f.path,
        old_path: f.old_path,
        status: f.status,
        additions: f.additions,
        deletions: f.deletions,
        blob_sha: "",
        viewed: false,
        viewed_stale: false,
        open_annotations: 0,
      }));
    }
    return filesQ.data?.files ?? [];
  }, [psRange, interdiffQ.data, filesQ.data]);
  const stops = orderQ.data === null || orderQ.data === undefined ? null : orderQ.data.stops;
  const ordered = useMemo(() => orderedRows(files, stops), [files, stops]);
  const paths = useMemo(() => ordered.map((f) => f.path), [ordered]);

  // PRR-U3 — `t`/`T` stop list: severity-ordered findings then comments,
  // per file, in `paths` order, overlay-filtered.
  const threadStops = useMemo(
    () => buildThreadStops(paths, threadsByPath, findingSeverityByThreadId, overlay),
    [paths, threadsByPath, findingSeverityByThreadId, overlay],
  );

  // PRR-F (design-ui.md §12.3, "Guided review tour") — fuses reading-order
  // (`paths`, already computed above) with findings (`findingsQ.data`,
  // already fetched for the overlay) into one flat stop list; NO new fetch.
  // State is client-only: `?tour=1` seeds the INITIAL on/off, the step
  // index lives in plain component state (this unit's own brief).
  const tourStops = useMemo(
    () => buildTourStops(paths, findingsQ.data?.findings ?? []),
    [paths, findingsQ.data],
  );
  const [tourOn, setTourOnState] = useState(() => searchParams.get("tour") === "1");
  const [tourStepIdx, setTourStepIdx] = useState(0);

  const [keys, setKeys] = useState<DiffKeysState>(initialDiffKeysState);
  const [hunkMap, setHunkMap] = useState<Record<string, number>>({});
  const hunkCounts = useMemo(() => paths.map((p) => hunkMap[p] ?? 0), [paths, hunkMap]);

  // --- V73-K2a — diff v2 page state ------------------------------------
  //
  // Everything DURABLE is in the URL (`?ps=`/`?ctx=`/`?noise=`/`?map=`/
  // `?file=`/`?hunk=`). What lives in component state below is either
  // fetched-and-derived (`parsedByPath`) or an ephemeral per-visit
  // interaction (a fold, a click-to-expand) that a reload is entitled to
  // reset. Drafts are the one exception and they carry their own storage
  // with a stated lifetime (`lib/reviewDrafts.ts`).
  const [parsedByPath, setParsedByPath] = useState<Map<string, ParsedDiff>>(new Map());
  const [folded, setFolded] = useState<Set<string>>(new Set());
  const [expandByHunk, setExpandByHunk] = useState<Map<string, ExpandRequest>>(new Map());
  const [drafts, setDrafts] = useState<DraftsState>(EMPTY_DRAFTS);
  const [draftsOpen, setDraftsOpen] = useState(false);
  const [publishing, setPublishing] = useState(false);

  // Drafts are per (repo, review) and restored on mount — the reload
  // survival the brief asks for, and the reason the tray says "this tab".
  useEffect(() => {
    if (!idOk) return;
    setDrafts(loadDrafts(repo, id));
  }, [repo, id, idOk]);
  useEffect(() => {
    if (!idOk) return;
    saveDrafts(repo, id, drafts);
  }, [repo, id, idOk, drafts]);

  const onParsed = useCallback((path: string, parsed: ParsedDiff) => {
    setParsedByPath((m) => (m.get(path) === parsed ? m : new Map(m).set(path, parsed)));
  }, []);

  // The moved-block index is page-wide by nature (a block "moved" INTO
  // another file), so it lives here and is honest about being computed
  // over the files actually loaded — `lib/diffNoise.ts`'s own header.
  const movedIndex: MovedIndex | null = useMemo(
    () => (parsedByPath.size === 0 ? null : buildMovedIndex(parsedByPath)),
    [parsedByPath],
  );
  const fileNoiseByPath = useMemo(() => {
    const m = new Map<string, NoiseLabel[]>();
    for (const f of files) {
      m.set(
        f.path,
        classifyFile({
          path: f.path,
          status: f.status,
          additions: f.additions,
          deletions: f.deletions,
        }),
      );
    }
    return m;
  }, [files]);

  /// The page-wide noise census — every hunk the page has PARSED, with its
  /// labels. Reported in the toolbar so "collapse noise" never hides a
  /// number.
  const noiseStats = useMemo(() => {
    const perHunk: NoiseLabel[][] = [];
    for (const [path, parsed] of parsedByPath) {
      const fileLabels = fileNoiseByPath.get(path) ?? [];
      for (const hunk of parsed.hunks) {
        perHunk.push(classifyHunk(path, hunk, fileLabels, movedIndex));
      }
    }
    return noiseCensus(perHunk);
  }, [parsedByPath, fileNoiseByPath, movedIndex]);

  const hunkViewedSet = useMemo(
    () => new Set((filesQ.data?.hunks_viewed ?? []).map((h) => h.hunk_id)),
    [filesQ.data],
  );
  const draftsByPath = useMemo(() => draftCountByPath(drafts), [drafts]);

  const apply = useCallback(
    (action: DiffKeysAction) => {
      setKeys((s) => reduceDiffKeys(s, action, hunkCounts));
    },
    [hunkCounts],
  );

  useEffect(() => {
    setKeys((s) => reduceDiffKeys(s, { type: "setFiles", files: paths }, hunkCounts));
    // hunkCounts omitted: setFiles ignores them; listing them would reset the cursor on every fetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paths]);

  const seeded = useRef(false);
  useEffect(() => {
    if (seeded.current || paths.length === 0) return;
    const hint = focusPath || fileHint;
    if (hint) {
      const idx = paths.indexOf(hint);
      if (idx >= 0) apply({ type: "gotoFile", fileIdx: idx });
    }
    seeded.current = true;
  }, [paths, focusPath, fileHint, apply]);

  // Single-file URL is the source of truth for the cursor file (not hunk).
  useEffect(() => {
    if (!single || !focusPath || paths.length === 0) return;
    const idx = paths.indexOf(focusPath);
    if (idx >= 0 && idx !== keys.cursor.fileIdx) apply({ type: "gotoFile", fileIdx: idx });
  }, [single, focusPath, paths, apply, keys.cursor.fileIdx]);

  // PRR-F — landing directly on `?tour=1` (e.g. `ReviewHeader`'s "Guided
  // tour" link) seeds `tourOn` from the URL but has never actually STEPPED
  // to stop 0 — one-shot, same ref-guarded seeding idiom as `seeded` above,
  // fires once `tourStops` has something to land on.
  const seededTour = useRef(false);
  useEffect(() => {
    if (seededTour.current || tourStops.length === 0) return;
    seededTour.current = true;
    if (tourOn) goToTourStep(0);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tourStops]);

  const onHunks = useCallback((path: string, n: number) => {
    setHunkMap((m) => (m[path] === n ? m : { ...m, [path]: n }));
  }, []);

  const viewedCount = files.filter((f) => f.viewed && !f.viewed_stale).length;
  const filesCount = files.length;
  const pct = filesCount > 0 ? Math.round((viewedCount / filesCount) * 100) : 0;

  const baseSha = filesQ.data?.base_sha;
  const tipSha = filesQ.data?.tip_sha;
  const review = reviewQ.data;
  const title = review?.title?.trim() || review?.head_ref || `Review #${id}`;
  const activePsNum = filesQ.data?.ps_number;

  const [helpOpen, setHelpOpen] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);
  const [jump, setJump] = useState("");
  const fileRollup = useMemo(
    () => (commentsQ.data ? indexThreads(commentsQ.data).rollup.perFile : null),
    [commentsQ.data],
  );
  const jumpHits = useMemo(
    () => speedFilterItems(ordered, jump, (f) => f.path + (f.old_path ? ` ${f.old_path}` : "")),
    [ordered, jump],
  );

  function withQuery(href: string): string {
    const q = searchParams.toString();
    return q ? `${href}?${q}` : href;
  }

  function setView(next: DiffMode) {
    saveDiffMode(next);
    setPrefMode(next);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("view", next);
    setSearchParams(nextParams, { replace: true });
  }

  async function toggleViewed(file: ReviewFileRow) {
    try {
      if (file.viewed && !file.viewed_stale) {
        await delViewed.mutateAsync(file.path);
      } else {
        await putViewed.mutateAsync({ path: file.path, blob_sha: file.blob_sha });
      }
    } catch (err) {
      toast.err(`couldn't update viewed: ${msg(err)}`);
    }
  }

  function goCockpit() {
    navigate(reviewUrl(repo, id));
  }

  function goFile(idx: number) {
    const path = paths[idx];
    if (!path) return;
    if (single) {
      navigate(withQuery(reviewDiffHref(repo, id, path)));
      return;
    }
    apply({ type: "gotoFile", fileIdx: idx });
  }

  // PRR-U3 — overlay cycle (`o`) + t/T thread-or-finding stepping + the
  // disposition menu (`d`, then a/d/w/f) + finding permalink copy (`y`).
  function setOverlay(next: OverlayMode) {
    const nextParams = new URLSearchParams(searchParams);
    const v = overlayParamValue(next);
    if (v) nextParams.set("overlay", v);
    else nextParams.delete("overlay");
    setSearchParams(nextParams, { replace: true });
  }

  // --- V73-K2a — the five URL writers -----------------------------------
  //
  // ONE helper, so every diff-v2 control writes its param the same way and
  // "the URL is the state" cannot rot into "the URL is the state, except
  // this control". `replace: true` matches every pre-existing writer on
  // this route (view/overlay/tour): a view knob is not a navigation step,
  // and Back must still leave the diff rather than undo a dial click.
  const setParam = useCallback(
    (key: string, value: string | null) => {
      const next = new URLSearchParams(searchParams);
      if (value === null) next.delete(key);
      else next.set(key, value);
      setSearchParams(next, { replace: true });
    },
    [searchParams, setSearchParams],
  );

  const setPs = useCallback(
    (next: DiffPsSelection | null) => setParam("ps", next === null ? null : formatDiffPs(next)),
    [setParam],
  );
  const setCtx = useCallback(
    (next: DiffCtxDial) => setParam("ctx", next === 3 ? null : String(next)),
    [setParam],
  );
  const setNoiseMode = useCallback(
    (next: NoiseMode) => setParam("noise", next === "shown" ? null : next),
    [setParam],
  );
  const setMapOpen = useCallback(
    (next: boolean) => setParam("map", next ? null : "0"),
    [setParam],
  );

  /// `] p` / `[ p` — step the HEAD patchset. A range keeps its base and
  /// moves its head, so stepping never silently collapses an interdiff
  /// into a plain patchset view.
  const stepPs = useCallback(
    (delta: 1 | -1) => {
      const list = (reviewQ.data?.patchsets ?? []).map((p) => p.ps_number).sort((a, b) => a - b);
      if (list.length === 0) return;
      const latest = list[list.length - 1];
      const curHead = psSel === null ? latest : typeof psSel === "number" ? psSel : psSel.to;
      const i = list.indexOf(curHead);
      const nextHead = list[(i === -1 ? list.length - 1 : i) + delta];
      if (nextHead === undefined) return;
      if (psRange) {
        if (psRange.from >= nextHead) return;
        setPs({ from: psRange.from, to: nextHead });
      } else {
        setPs(nextHead === latest ? null : nextHead);
      }
    },
    [reviewQ.data, psSel, psRange, setPs],
  );

  const toggleFold = useCallback((hid: string) => {
    setFolded((cur) => {
      const next = new Set(cur);
      if (next.has(hid)) next.delete(hid);
      else next.add(hid);
      return next;
    });
  }, []);
  const setFold = useCallback((hid: string, want: boolean) => {
    setFolded((cur) => {
      if (cur.has(hid) === want) return cur;
      const next = new Set(cur);
      if (want) next.add(hid);
      else next.delete(hid);
      return next;
    });
  }, []);

  const expandHunkBy = useCallback((hid: string, dir: "up" | "down") => {
    setExpandByHunk((cur) => {
      const prev = cur.get(hid) ?? { before: 0, after: 0 };
      const next = new Map(cur);
      next.set(
        hid,
        dir === "up"
          ? { before: prev.before + EXPAND_STEP, after: prev.after }
          : { before: prev.before, after: prev.after + EXPAND_STEP },
      );
      return next;
    });
  }, []);

  const toggleHunkViewed = useCallback(
    (path: string, hid: string) => {
      const viewed = hunkViewedSet.has(hid);
      void (async () => {
        try {
          if (viewed) await delHunkViewed.mutateAsync(hid);
          else await putHunkViewed.mutateAsync({ hunkId: hid, path });
        } catch (err) {
          toast.err(`couldn't update hunk viewed: ${msg(err)}`);
        }
      })();
    },
    [hunkViewedSet, delHunkViewed, putHunkViewed],
  );

  // --- V73-K2a — drafts --------------------------------------------------
  const draftCreate = useCallback(
    async (
      path: string,
      side: DiffSide,
      line: number,
      lineEnd: number | undefined,
      body: string,
      intent: string,
    ) => {
      const draft: ReviewDraft = {
        id: newDraftId(),
        path,
        side,
        line,
        lineEnd,
        // The composer's own intent select offers the full annotation
        // vocabulary; a draft carries only the two the atomic batch can
        // publish, so anything else is recorded as a plain comment rather
        // than silently posted under a different intent.
        intent: intent === "question" ? "question" : "comment",
        body,
        createdAt: Date.now(),
      };
      setDrafts((cur) => upsertDraft(cur, draft));
      setDraftsOpen(true);
      toast.ok("Drafted — publish when the review is ready");
    },
    [],
  );

  const publishDrafts = useCallback(() => {
    if (drafts.drafts.length === 0 || publishing) return;
    void (async () => {
      setPublishing(true);
      try {
        // ONE request, ONE store transaction, at most ONE SSE. A refusal
        // writes nothing, so the tray is only cleared after a success.
        const psNumber =
          psSel === null ? null : typeof psSel === "number" ? psSel : psSel.to;
        const res = await postAnnotationsBatch(
          draftsToBatch(repo, id, psNumber, drafts.drafts),
        );
        setDrafts((cur) => clearDrafts(cur));
        setDraftsOpen(false);
        toast.ok(`Published ${res.applied} draft${res.applied === 1 ? "" : "s"}`);
        void queryClient.invalidateQueries({ queryKey: ["reviews", repo] });
      } catch (err) {
        toast.err(`publish failed — nothing was written: ${msg(err)}`);
      } finally {
        setPublishing(false);
      }
    })();
  }, [drafts, publishing, psSel, repo, id, queryClient]);

  const discardDrafts = useCallback(() => {
    if (drafts.drafts.length === 0) return;
    void (async () => {
      const ok = await confirm({
        title: "Discard every draft?",
        body: `${drafts.drafts.length} unpublished draft(s) will be deleted. This cannot be undone.`,
        confirmLabel: "Discard",
        danger: true,
      });
      if (!ok) return;
      setDrafts((cur) => clearDrafts(cur));
    })();
  }, [drafts, confirm]);

  const [focusThreadId, setFocusThreadId] = useState<string | null>(null);
  const seededFocus = useRef(false);
  useEffect(() => {
    if (seededFocus.current) return;
    if (threadId) {
      seededFocus.current = true;
      setFocusThreadId(threadId);
      return;
    }
    if (!findingParam) {
      seededFocus.current = true;
      return;
    }
    // Waiting on the findings fetch to resolve the slug -> annotation id;
    // `findingsQ.data` becoming available is the "resolved" signal (a miss
    // still marks seeded — nothing to focus, but never retries forever).
    if (findingsQ.data) {
      seededFocus.current = true;
      const f = findingsSlugMap.get(findingParam);
      if (f) setFocusThreadId(f.annotation_id);
    }
  }, [threadId, findingParam, findingsQ.data, findingsSlugMap]);

  function stepThread(dir: 1 | -1) {
    const next = stepThreadStop(threadStops, focusThreadId, dir);
    if (!next) return;
    setFocusThreadId(next.id);
    if (single && next.path !== focusPath) {
      goFile(next.fileIdx);
    } else if (!single && next.fileIdx !== keys.cursor.fileIdx) {
      apply({ type: "gotoFile", fileIdx: next.fileIdx });
    }
  }

  // PRR-F (design-ui.md §12.3) — guided tour. Reuses `goFile`/`apply`/
  // `setFocusThreadId`/`putViewed` verbatim (this unit's own brief: "REUSE
  // U3's key plumbing, do not fork it") — a tour stop is just a
  // (fileIdx, optional annotationId) pair driven through the SAME
  // machinery `n`/`p`/`t` already use.
  function goToTourStep(idx: number) {
    const stop = tourStops[idx];
    if (!stop) return;
    setTourStepIdx(idx);
    const fileIdx = paths.indexOf(stop.path);
    if (fileIdx >= 0) {
      if (single) goFile(fileIdx);
      else if (fileIdx !== keys.cursor.fileIdx) apply({ type: "gotoFile", fileIdx });
    }
    if (stop.annotationId) setFocusThreadId(stop.annotationId);
    // Auto-mark-viewed per file (design-ui.md §12.3: "auto-mark viewed per
    // file") — the existing PUT, never a new mutation.
    const row = ordered.find((f) => f.path === stop.path);
    if (row && !(row.viewed && !row.viewed_stale)) {
      void putViewed.mutateAsync({ path: row.path, blob_sha: row.blob_sha });
    }
  }
  function startTour() {
    setTourOnState(true);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set("tour", "1");
    setSearchParams(nextParams, { replace: true });
    goToTourStep(0);
  }
  function exitTour() {
    setTourOnState(false);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.delete("tour");
    setSearchParams(nextParams, { replace: true });
  }
  function advanceTour(dir: 1 | -1) {
    const next = clampTourStep(tourStepIdx + dir, tourStops.length);
    if (next == null || next === tourStepIdx) return;
    goToTourStep(next);
  }

  const [dispositionMenuOpen, setDispositionMenuOpen] = useState(false);
  function openDispositionMenu() {
    if (!focusThreadId || !findingsById.has(focusThreadId)) return;
    setDispositionMenuOpen(true);
  }
  function closeDispositionMenu() {
    setDispositionMenuOpen(false);
  }
  async function applyDispositionFromMenu(d: FindingDisposition) {
    setDispositionMenuOpen(false);
    if (!focusThreadId) return;
    const f = findingsById.get(focusThreadId);
    if (!f) return;
    try {
      const active = f.disposition?.state === d;
      if (active) await dispositionMut.clearDisposition.mutateAsync(f.slug);
      else await dispositionMut.setDisposition.mutateAsync({ slug: f.slug, disposition: d });
      toast.ok(`${f.slug}: ${active ? "disposition cleared" : dispositionLabel(d)}`);
    } catch (err) {
      toast.err(`couldn't update disposition: ${msg(err)}`);
    }
  }

  async function copyFocusedFindingPermalink() {
    if (!focusThreadId) return;
    const f = findingsById.get(focusThreadId);
    if (!f) return;
    const href = reviewDiffHref(repo, id, f.location.path, { finding: f.slug });
    try {
      await navigator.clipboard.writeText(`${window.location.origin}${href}`);
      toast.ok("Finding permalink copied");
    } catch {
      toast.err("couldn't copy permalink");
    }
  }

  const cursorPath = keys.files[keys.cursor.fileIdx] ?? "";
  const cursorRow = ordered.find((f) => f.path === cursorPath);

  // --- V73-K2a — the cursor hunk, and `?hunk=`'s two directions ----------
  //
  // The hunk cursor is `keys.cursor.hunkIdx` (unchanged — `j`/`k` already
  // drove it). What is new is that it has a NAME on the wire: the hunk's
  // content address. `?hunk=` seeds the cursor once on landing, and every
  // cursor move writes it back, so a copied URL reopens the same CHANGE
  // even after a rebase renumbers the file.
  const cursorParsed = parsedByPath.get(cursorPath) ?? null;
  const cursorHunkId = useMemo(() => {
    const hunk = cursorParsed?.hunks[keys.cursor.hunkIdx];
    return hunk ? hunkId(cursorPath, hunk) : null;
  }, [cursorParsed, cursorPath, keys.cursor.hunkIdx]);

  const seededHunk = useRef(false);
  useEffect(() => {
    if (seededHunk.current || !hunkParam || parsedByPath.size === 0) return;
    for (const [path, parsed] of parsedByPath) {
      const idx = parsed.hunks.findIndex((h) => hunkId(path, h) === hunkParam);
      if (idx === -1) continue;
      seededHunk.current = true;
      const fileIdx = paths.indexOf(path);
      if (fileIdx >= 0) {
        setKeys((st) => {
          const at = reduceDiffKeys(st, { type: "gotoFile", fileIdx }, hunkCounts);
          return { ...at, cursor: { fileIdx, hunkIdx: idx } };
        });
      }
      return;
    }
  }, [hunkParam, parsedByPath, paths, hunkCounts]);

  // Cursor → URL. Deliberately `replace`, like every other view writer on
  // this route: stepping hunks must not fill the history stack.
  const lastWrittenHunk = useRef<string | null>(null);
  useEffect(() => {
    if (!seededHunk.current && hunkParam) return;
    if (cursorHunkId === null || lastWrittenHunk.current === cursorHunkId) return;
    lastWrittenHunk.current = cursorHunkId;
    setParam("hunk", cursorHunkId);
  }, [cursorHunkId, hunkParam, setParam]);

  const mapOpen = mapParamOpen && !isMobile;
  const chapters = useMemo(() => mapChapters(ordered, stops), [ordered, stops]);
  const findingsCountByPath = useMemo(() => {
    const m = new Map<string, number>();
    for (const f of findingsQ.data?.findings ?? []) {
      m.set(f.location.path, (m.get(f.location.path) ?? 0) + 1);
    }
    return m;
  }, [findingsQ.data]);
  const mapStateByPath = useMemo(() => {
    const m = new Map<string, MapRowState>();
    for (const f of ordered) {
      m.set(f.path, {
        path: f.path,
        viewed: f.viewed,
        viewedStale: f.viewed_stale,
        openComments: fileRollup?.get(f.path)?.open ?? f.open_annotations,
        findings: findingsCountByPath.get(f.path) ?? 0,
        drafts: draftsByPath.get(f.path) ?? 0,
        noise: (fileNoiseByPath.get(f.path) ?? []).map((l) => l.cls),
      });
    }
    return m;
  }, [ordered, fileRollup, findingsCountByPath, draftsByPath, fileNoiseByPath]);

  const v2: DiffV2Api = useMemo(
    () => ({
      ctx: ctxDial,
      noiseMode,
      movedIndex,
      fileNoise: [],
      hunkViewed: hunkViewedSet,
      folded,
      expand: expandByHunk,
      drafts,
      currentHunk: null,
      onParsed,
      onToggleHunkViewed: toggleHunkViewed,
      onToggleFold: toggleFold,
      onExpandHunk: expandHunkBy,
      onDraftCreate: draftCreate,
    }),
    [
      ctxDial,
      noiseMode,
      movedIndex,
      hunkViewedSet,
      folded,
      expandByHunk,
      drafts,
      onParsed,
      toggleHunkViewed,
      toggleFold,
      expandHunkBy,
      draftCreate,
    ],
  );
  /// Per-file specialisation of `v2` — the file's own noise labels and
  /// whether the hunk cursor is inside it. One object per rendered file,
  /// built from the shared bag rather than a second derivation.
  const v2For = useCallback(
    (path: string): DiffV2Api => ({
      ...v2,
      fileNoise: fileNoiseByPath.get(path) ?? [],
      currentHunk: path === cursorPath ? keys.cursor.hunkIdx : null,
    }),
    [v2, fileNoiseByPath, cursorPath, keys.cursor.hunkIdx],
  );

  // Keep latest values for the window keydown handler without rebinding.
  const live = useRef({
    apply,
    keys,
    hunkCounts,
    ordered,
    paths,
    single,
    mode,
    helpOpen,
    cursorRow,
    goCockpit,
    goFile,
    setView,
    toggleViewed,
    setHelpOpen,
    overlay,
    setOverlay,
    stepThread,
    focusThreadId,
    dispositionMenuOpen,
    openDispositionMenu,
    closeDispositionMenu,
    applyDispositionFromMenu,
    copyFocusedFindingPermalink,
    tourOn,
    advanceTour,
    exitTour,
  });
  live.current = {
    apply,
    keys,
    hunkCounts,
    ordered,
    paths,
    single,
    mode,
    helpOpen,
    cursorRow,
    goCockpit,
    goFile,
    setView,
    toggleViewed,
    setHelpOpen,
    overlay,
    setOverlay,
    stepThread,
    focusThreadId,
    dispositionMenuOpen,
    openDispositionMenu,
    closeDispositionMenu,
    applyDispositionFromMenu,
    copyFocusedFindingPermalink,
    tourOn,
    advanceTour,
    exitTour,
  };

  // V70-A5 — the diff's keys, as kbc-cmd/1 handlers.
  //
  // `useCommandScope` publishes the live context so the palette, the `?`
  // sheet and which-key all know what is available RIGHT NOW; the handlers
  // below are the execution half (the two-layer rule: the registry declares,
  // the surface that renders the thing runs it).
  //
  // Two modal gates survive, and stay LOCAL because they are facts about this
  // component's own state: with the help sheet up, only its dismissal is
  // read; with the disposition menu open, only a/d/w/f and its dismissal are.
  // The registry expresses the second one as `when: diff.menu` / `!diff.menu`
  // so the palette and the conflicts gate see it too.
  /// The `c`/`C` compose action, lifted out of the old switch: find the
  /// cursor row's line number on the requested side and open the composer
  /// there. Unchanged behaviour — only its home moved.
  function composeAtCursor(want: DiffSide) {
    const L = live.current;
    const path = L.keys.files[L.keys.cursor.fileIdx];
    if (!path) return;
    const root = document.querySelector(`[data-kbc-rdiff-file="${cssAttr(path)}"]`);
    if (!root) return;
    const current = root.querySelector("[data-kbc-diff-current]") ?? root;
    const attr = want === "old" ? "data-old-line" : "data-new-line";
    const el = current.querySelector(`[${attr}]`) ?? root.querySelector(`[${attr}]`);
    const raw = el?.getAttribute(attr);
    const n = raw ? Number(raw) : NaN;
    if (!Number.isFinite(n) || n < 1) return;
    setComposeReq({ path, side: want, line: n, token: Date.now() });
  }

  useCommandScope("diff", {
    "diff.menu": dispositionMenuOpen,
    "diff.tour": tourOn,
    "help.open": helpOpen,
  });

  function gated(fn: () => void): () => void {
    return () => {
      const L = live.current;
      if (L.helpOpen || L.dispositionMenuOpen) return;
      fn();
    };
  }
  function nextFile(delta: 1 | -1) {
    const L = live.current;
    if (L.single) L.goFile(L.keys.cursor.fileIdx + delta);
    else L.apply({ type: delta === 1 ? "nextFile" : "prevFile" });
  }

  useCommandHandlers({
    "diff.hunk-next": gated(() => live.current.apply({ type: "nextHunk" })),
    "diff.hunk-prev": gated(() => live.current.apply({ type: "prevHunk" })),
    "diff.file-next": gated(() => nextFile(1)),
    "diff.file-prev": gated(() => nextFile(-1)),
    "diff.file-first": gated(() => live.current.apply({ type: "firstFile" })),
    "diff.file-last": gated(() => live.current.apply({ type: "lastFile" })),
    "diff.toggle-viewed": gated(() => {
      const L = live.current;
      if (L.cursorRow) void L.toggleViewed(L.cursorRow);
    }),
    "diff.viewed-advance": gated(() => {
      const L = live.current;
      const row = L.cursorRow;
      if (!row) return;
      void (async () => {
        if (!(row.viewed && !row.viewed_stale)) await L.toggleViewed(row);
        const viewed = new Set(
          L.ordered.filter((f) => f.viewed && !f.viewed_stale).map((f) => f.path),
        );
        viewed.add(row.path);
        const next = nextUnviewedFileIdx(L.paths, viewed, L.keys.cursor.fileIdx);
        if (next != null) L.goFile(next);
      })();
    }),
    "diff.compose-new": gated(() => composeAtCursor("new")),
    "diff.compose-old": gated(() => composeAtCursor("old")),
    "diff.split-toggle": gated(() => {
      const L = live.current;
      L.setView(L.mode === "split" ? "unified" : "split");
    }),
    "diff.collapse": gated(() => live.current.apply({ type: "toggleCollapse" })),
    "diff.thread-next": gated(() => live.current.stepThread(1)),
    "diff.thread-prev": gated(() => live.current.stepThread(-1)),
    "diff.overlay-cycle": gated(() => {
      const L = live.current;
      L.setOverlay(nextOverlayMode(L.overlay));
    }),
    "diff.disposition": gated(() => live.current.openDispositionMenu()),
    "diff.permalink-copy": gated(() => void live.current.copyFocusedFindingPermalink()),
    "diff.tour-next": gated(() => live.current.advanceTour(1)),
    "diff.tour-prev": gated(() => live.current.advanceTour(-1)),
    // --- V73-K2a — diff v2's twelve rows -------------------------------
    //
    // All `dispatch: "surface"`, all registered HERE, which is what keeps
    // `commands/deadRows.test.ts`'s ledger at zero and what
    // `web-code/CLAUDE.md`'s keyboard section asks a new row's author to
    // check before trusting the key does anything.
    "diff.hunk-viewed": gated(() => {
      if (cursorHunkId) toggleHunkViewed(cursorPath, cursorHunkId);
    }),
    "diff.fold": gated(() => {
      if (cursorHunkId) setFold(cursorHunkId, true);
    }),
    "diff.unfold": gated(() => {
      if (cursorHunkId) setFold(cursorHunkId, false);
    }),
    "diff.fold-toggle": gated(() => {
      if (cursorHunkId) toggleFold(cursorHunkId);
    }),
    "diff.context-cycle": gated(() => setCtx(nextDiffCtx(ctxDial))),
    "diff.noise-toggle": gated(() =>
      setNoiseMode(noiseMode === "collapsed" ? "shown" : "collapsed"),
    ),
    "diff.map-toggle": gated(() => setMapOpen(!mapParamOpen)),
    "diff.ps-next": gated(() => stepPs(1)),
    "diff.ps-prev": gated(() => stepPs(-1)),
    "diff.drafts": gated(() => setDraftsOpen((v) => !v)),
    "diff.publish": gated(() => publishDrafts()),
    "diff.drafts-discard": gated(() => discardDrafts()),
    // The four menu picks are `when: diff.menu` rows, so they only resolve
    // while the menu is up — no local gate needed, and no swallow: any other
    // key is blocked by `gated` above rather than by a blanket preventDefault.
    "diff.disposition.agree": () => void live.current.applyDispositionFromMenu("agree"),
    "diff.disposition.dispute": () => void live.current.applyDispositionFromMenu("dispute"),
    "diff.disposition.waive": () => void live.current.applyDispositionFromMenu("waive"),
    "diff.disposition.fix-later": () => void live.current.applyDispositionFromMenu("fix-later"),
    // The Esc class. Innermost first, and NEVER navigation: the old
    // `Escape → goCockpit()` third branch is gone — leaving the page is `u`
    // (`nav.back`) or the browser's own Back, both of which restore the
    // scroll position Esc used to throw away.
    "dismiss.help": () => live.current.setHelpOpen(false),
    "dismiss.menu": () => live.current.closeDispositionMenu(),
    "dismiss.mode": () => {
      const L = live.current;
      if (L.tourOn) L.exitTour();
    },
    "help.keys": () => live.current.setHelpOpen(!live.current.helpOpen),
  });

  // Cursor row: mark + scroll. Collapsed / unfetched files mark the section header.
  useEffect(() => {
    const path = keys.files[keys.cursor.fileIdx];
    if (!path) return;
    const root = document.querySelector(`[data-kbc-rdiff-file="${cssAttr(path)}"]`);
    if (!root) return;
    document.querySelectorAll("[data-kbc-diff-current]").forEach((el) => {
      el.removeAttribute("data-kbc-diff-current");
    });
    const hunks = root.querySelectorAll(".kbc-diff__hunk, .kbc-sdiff__hunk");
    const header = root.querySelector("[data-kbc-rdiff-section]");
    const target =
      (hunks[keys.cursor.hunkIdx] as HTMLElement | undefined) ??
      (header as HTMLElement | null) ??
      (root as HTMLElement);
    target.setAttribute("data-kbc-diff-current", "");
    target.scrollIntoView({ block: "nearest" });
  }, [keys.cursor.fileIdx, keys.cursor.hunkIdx, keys.files, hunkCounts, mode, keys.collapsed]);

  // `?thread=`/`?finding=` deep-link + PRR-U3's `t`/`T` stepping: scroll to
  // the focused thread (or its resolved line), expand it (DiffThread
  // watches flashThreadId), flash. Generalized from the pre-PRR-U3
  // one-shot "flash the URL's ?thread= once" effect to re-fire on every
  // `focusThreadId` CHANGE (guarded by `lastFlashed` so it never re-flashes
  // the SAME id twice) — `t`/`T` repeatedly change `focusThreadId`, so this
  // is the same machinery, just no longer single-shot.
  const lastFlashed = useRef<string | null>(null);
  useEffect(() => {
    if (!focusThreadId || lastFlashed.current === focusThreadId) return;
    const el = document.querySelector(`[data-kbc-review-thread="${cssAttr(focusThreadId)}"]`);
    if (!el) return;
    lastFlashed.current = focusThreadId;
    el.classList.add("kbc-rdiff__flash");
    el.scrollIntoView({ block: "center" });
    const t = window.setTimeout(() => el.classList.remove("kbc-rdiff__flash"), 1400);
    return () => window.clearTimeout(t);
  }, [focusThreadId, commentsQ.data, hunkCounts, mode]);

  // `?line=N&side=` deep-link: scroll + flash. Works in both unified and split
  // (rows carry data-old-line / data-new-line).
  const flashed = useRef(false);
  useEffect(() => {
    if (flashed.current || line == null || side == null) return;
    const path = focusPath || fileHint || paths[keys.cursor.fileIdx];
    if (!path) return;
    const root = document.querySelector(`[data-kbc-rdiff-file="${cssAttr(path)}"]`);
    if (!root) return;
    const attr = side === "old" ? "data-old-line" : "data-new-line";
    const row = root.querySelector(`[${attr}="${line}"]`);
    if (!row) return;
    flashed.current = true;
    row.classList.add("kbc-rdiff__flash");
    row.scrollIntoView({ block: "center" });
    const t = window.setTimeout(() => row.classList.remove("kbc-rdiff__flash"), 1400);
    return () => window.clearTimeout(t);
  }, [line, side, focusPath, fileHint, paths, keys.cursor.fileIdx, hunkCounts, mode]);

  // `?file=` all-files scroll hint (once the section exists).
  const fileScrolled = useRef(false);
  useEffect(() => {
    if (fileScrolled.current || single || !fileHint) return;
    const el = document.querySelector(`[data-kbc-rdiff-file="${cssAttr(fileHint)}"]`);
    if (!el) return;
    fileScrolled.current = true;
    el.scrollIntoView({ block: "start" });
  }, [fileHint, single, paths]);

  if (!idOk) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">Invalid review id.</div>;
  }
  if (reviewQ.isLoading || filesQ.isLoading) {
    return <div className="kbc-reader__hint">Loading review…</div>;
  }
  if (reviewQ.error || !review) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error">
        {(reviewQ.error as Error | undefined)?.message ?? "Review not found"}
      </div>
    );
  }
  if (filesQ.error) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error">{(filesQ.error as Error).message}</div>
    );
  }
  if (!baseSha || !tipSha) {
    return <div className="kbc-reader__hint">No patchset to diff.</div>;
  }

  const cursorIdx = keys.cursor.fileIdx;
  const hasPrev = cursorIdx > 0;
  const hasNext = cursorIdx < paths.length - 1;
  // PRR-U3 — the flash target is now `focusThreadId` (seeded from
  // `?thread=`/`?finding=`, then driven by `t`/`T`), not the raw URL param.
  const flashThreadId = focusThreadId;
  const composeFor = (path: string) =>
    composeReq && composeReq.path === path
      ? { side: composeReq.side, line: composeReq.line, token: composeReq.token }
      : null;

  return (
    <div
      className={"kbc-rdiff" + (single ? " kbc-rdiff--single" : " kbc-rdiff--all")}
      id="main"
      data-kbc-rdiff={id}
      data-kbc-rdiff-mode={single ? "single" : "all"}
      data-kbc-rdiff-thread={threadId ?? undefined}
    >
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
          patchsets={review?.patchsets ?? []}
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
            {reviewGithubThreadsQ.data ? ` · GitHub ${githubThreadCountTotal(reviewGithubThreadsQ.data.threads)}` : ""}
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
                    githubThreads={reviewGithubThreadsQ.data?.threads}
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
              githubThreads={reviewGithubThreadsQ.data?.threads}
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
      <MobileDrawer
        open={filesOpen}
        onClose={() => setFilesOpen(false)}
        title="Files"
        ariaLabel="Review files"
      >
        {ordered.map((file, idx) => {
          const checked = !!(file.viewed && !file.viewed_stale);
          const openN = fileRollup?.get(file.path)?.open ?? file.open_annotations;
          return (
            <button
              key={file.path}
              type="button"
              className={
                "kbc-rdiff__file-row" + (file.path === cursorPath ? " is-current" : "")
              }
              onClick={() => {
                goFile(idx);
                const el = document.querySelector(
                  `[data-kbc-rdiff-file="${cssAttr(file.path)}"]`,
                );
                el?.scrollIntoView({ block: "start" });
                setFilesOpen(false);
              }}
              data-kbc-rdiff-drawer-file={file.path}
            >
              <span
                className="kbc-rdiff__file-check"
                aria-label={checked ? "viewed" : "unviewed"}
                data-kbc-rdiff-drawer-viewed={checked ? "1" : "0"}
              >
                {checked ? <Icon.Check /> : null}
              </span>
              <span className="kbc-rdiff__file-row-path">{file.path}</span>
              {openN > 0 && (
                <span className="kbc-review__file-ann" data-kbc-rdiff-drawer-ann>
                  {openN}
                </span>
              )}
            </button>
          );
        })}
      </MobileDrawer>
      {dispositionMenuOpen && focusThreadId && findingsById.get(focusThreadId) && (
        <div
          className="kbc-rdiff__disp-menu-scrim"
          onClick={closeDispositionMenu}
          data-kbc-rdiff-disposition-menu
        >
          <div
            className="kbc-rdiff__disp-menu"
            role="dialog"
            aria-modal="true"
            aria-label="Set disposition"
            onClick={(e) => e.stopPropagation()}
          >
            <div className="kbc-rdiff__disp-menu-title">
              {findingsById.get(focusThreadId)?.slug} — set disposition
            </div>
            <ul className="kbc-rdiff__disp-menu-list">
              <li>
                <kbd>a</kbd> agree
              </li>
              <li>
                <kbd>d</kbd> dispute
              </li>
              <li>
                <kbd>w</kbd> waive
              </li>
              <li>
                <kbd>f</kbd> fix-later
              </li>
              <li>
                <kbd>Esc</kbd> cancel
              </li>
            </ul>
          </div>
        </div>
      )}
      <DraftsTray
        open={draftsOpen}
        drafts={drafts.drafts}
        publishing={publishing}
        onClose={() => setDraftsOpen(false)}
        onGoTo={(d) => {
          const idx = paths.indexOf(d.path);
          if (idx >= 0) goFile(idx);
          document
            .querySelector(`[data-kbc-rdiff-file="${cssAttr(d.path)}"]`)
            ?.scrollIntoView({ block: "start" });
        }}
        onRemove={(draftId) => setDrafts((cur) => removeDraft(cur, draftId))}
        onPublish={publishDrafts}
        onDiscardAll={discardDrafts}
      />
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} context="review-diff" />
    </div>
  );
}
