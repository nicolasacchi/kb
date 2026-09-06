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
import { useInViewOnce } from "../hooks/useInViewOnce";
import {
  useReviewComments,
  useReviewDiffComments,
  useReviewFindingDispositionMutations,
} from "../hooks/useReviewComments";
import {
  useDeleteReviewViewed,
  useGithubThreads,
  usePutReviewViewed,
  useReview,
  useReviewFiles,
  useReviewFindings,
  useReviewImpact,
  useReviewReadingOrder,
} from "../hooks/useReviews";
import { indexThreads, type DiffSide } from "../lib/reviewComments";
import { buildDiagnosticsView, diagnosticGutterMarks, diagnosticsChipText } from "../lib/diagnostics";
import { parseUnifiedDiff } from "../lib/diff";
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
import { codeUrl, reviewDiffHref, reviewUrl } from "../lib/codeUrl";
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
}) {
  const navigate = useNavigate();
  const { data, isLoading, error } = useDiff(repo, path, from, to);
  const parsed = useMemo(() => (data ? parseUnifiedDiff(data.diff) : null), [data]);
  const comments = useReviewDiffComments(repo, reviewId, ps, path, { compose, flashThreadId, overlay });
  useEffect(() => {
    if (parsed) onHunks(path, parsed.hunks.length);
  }, [parsed, path, onHunks]);

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
    <DiffFile
      path={path}
      parsed={parsed}
      mode={mode}
      comments={comments}
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

  const psParam = searchParams.get("ps") ?? "latest";
  const psQuery = psParam === "" ? "latest" : psParam;
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

  const files = filesQ.data?.files ?? [];
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
          {psQuery === "latest" && activePsNum != null ? `ps${activePsNum}` : `ps${psQuery}`}
        </span>
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
      <div className="kbc-rdiff__body">
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
            />
          ))
        )}
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
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} context="review-diff" />
    </div>
  );
}
