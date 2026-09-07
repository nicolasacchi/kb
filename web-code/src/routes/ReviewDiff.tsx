import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import type { ReviewDetailPr, ReviewFileRow } from "../api/types";
import {
  useReviewComments,
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
  useReviewInterdiff,
  useReviewPseudoList,
  useReviewReadingOrder,
} from "../hooks/useReviews";
import { indexThreads, type DiffSide } from "../lib/reviewComments";
import type { ParsedDiff } from "../lib/diff";
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
  nextOverlayMode,
  type FindingDisposition,
} from "../lib/diffFindings";
import { buildTourStops, clampTourStep } from "../lib/reviewTour";
import { nextDiffCtx, reviewDiffHref, reviewUrl } from "../lib/codeUrl";
// V73-K2a — diff v2's four pure halves: hunk identity, noise labels, the
// context dial, the drafts tray. Each is unit-pinned in its own file; this
// route only wires them together.
import { hunkId } from "../lib/diffHunks";
import {
  buildMovedIndex,
  classifyFile,
  classifyHunk,
  noiseCensus,
  type MovedIndex,
  type NoiseLabel,
} from "../lib/diffNoise";
import { EXPAND_STEP, type ExpandRequest } from "../lib/diffContext";
import {
  clearDrafts,
  draftCountByPath,
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
import { pseudoPath } from "../lib/pseudoFiles";
import { postAnnotationsBatch } from "../api/client";
import { useConfirm } from "../components/ConfirmProvider";
import { useIsMobile } from "../hooks/useIsMobile";
import { useQueryClient } from "@tanstack/react-query";
import { speedFilterItems } from "../lib/speedSearch";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { toast } from "../lib/toast";
import "../styles/reviews.css";
// V73-K2b — the route's own center renderers and pure helpers, extracted
// verbatim. `ReviewDiff` below is the SHELL: queries, page-wide derivations,
// the URL writers and the command handlers.
import { type DiffV2Api } from "./reviewDiff/DiffSections";
import ReviewDiffCenter from "./reviewDiff/ReviewDiffCenter";
import ReviewDiffRail from "./reviewDiff/ReviewDiffRail";
import ReviewDiffToolbar from "./reviewDiff/ReviewDiffToolbar";
import { cssAttr, msg, orderedRows, splatPath } from "./reviewDiff/helpers";
import { useReviewDiffState } from "./reviewDiff/useReviewDiffState";

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


export default function ReviewDiff() {
  const { repo = "", id: idParam = "" } = useParams<{ repo: string; id: string }>();
  const focusPath = splatPath(useParams()["*"] ?? "");
  const single = focusPath.length > 0;
  const id = Number(idParam);
  const idOk = Number.isFinite(id) && id > 0;
  const navigate = useNavigate();
  // V73-K2b — every `?param=` this route reads or writes lives in ONE hook.
  // A `?ps=a..b` range is the INTERDIFF, which is a different route with a
  // different, thinner file row — see `files` below for the honest degrade
  // that entails.
  const {
    psSel,
    psRange,
    psQuery,
    ctxDial,
    noiseMode,
    mapParamOpen,
    hunkParam,
    line,
    side,
    fileHint,
    threadId,
    findingParam,
    overlay,
    mode,
    tourParamOn,
    setParam,
    setPs,
    setCtx,
    setNoiseMode,
    setMapOpen,
    setView,
    setOverlay,
    setTourParam,
    withQuery,
  } = useReviewDiffState();

  const reviewQ = useReview(repo, idOk ? id : undefined);
  const filesQ = useReviewFiles(repo, idOk ? id : undefined, psQuery);
  const orderQ = useReviewReadingOrder(repo, idOk ? id : undefined, true);
  // V73-K2c (kbc-pseudo/1) — the map column's chapter zero. 404→null degrade
  // (older server) mirrors every other optional review surface's own hook.
  const pseudoQ = useReviewPseudoList(repo, idOk ? id : undefined, psQuery, true);
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
  const [tourOn, setTourOnState] = useState(() => tourParamOn);
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
  // V73-K2c (kbc-hunk-turns/1) — at most ONE hunk's turns panel open at a
  // time, globally (a `kbc-hunkid/1` id already encodes its own path, so
  // there is never an ambiguity about which file it belongs to). Browser-
  // local, ephemeral state, same posture `folded`/`expandByHunk` above
  // already take — a reload is entitled to reset it, and mounting the panel
  // IS the fetch trigger (never auto-fetched for every hunk).
  const [turnsOpenId, setTurnsOpenId] = useState<string | null>(null);
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

  /// V73-K2c (kbc-pseudo/1) — "the map column's chapter zero" opener. A
  /// pseudo path is never one of `paths`/`ordered` (it is not a diffed
  /// file at all), so this ALWAYS navigates to the path-segment route
  /// (which is what puts the page in single-file-focus mode,
  /// `single = focusPath.length > 0` above) rather than reusing `goFile`'s
  /// index-based lookup.
  function openPseudo(name: string) {
    navigate(withQuery(reviewDiffHref(repo, id, pseudoPath(name))));
  }

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
    setTourParam(true);
    goToTourStep(0);
  }
  function exitTour() {
    setTourOnState(false);
    setTourParam(false);
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

  const onToggleTurns = useCallback((hunkId: string) => {
    setTurnsOpenId((cur) => (cur === hunkId ? null : hunkId));
  }, []);

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
      turnsOpenId,
      onToggleTurns,
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
      turnsOpenId,
      onToggleTurns,
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
    // V73-K2c — kbc-hunk-turns/1, the on-demand loopback join for the
    // CURSOR hunk (the same `cursorHunkId` the `Space h`/`z c` family
    // already act on).
    "diff.hunk-turns": gated(() => {
      if (cursorHunkId) onToggleTurns(cursorHunkId);
    }),
    // V73-K2c — kbc-pseudo/1, chapter zero. Four fixed rows rather than a
    // `Space {1-9}`-style wildcard (`drawer.tab`'s own precedent): these
    // name four SPECIFIC, permanent files, not an open-ended list, so a
    // dedicated id per file is the honest shape.
    "diff.pseudo.pr-body": gated(() => openPseudo("pr-body.md")),
    "diff.pseudo.review-md": gated(() => openPseudo("review.md")),
    "diff.pseudo.findings": gated(() => openPseudo("findings.json")),
    "diff.pseudo.commits": gated(() => openPseudo("commits.md")),
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
      <ReviewDiffToolbar
        repo={repo}
        id={id}
        title={title}
        patchsets={review?.patchsets ?? []}
        psSel={psSel}
        psRange={psRange}
        psQuery={psQuery}
        activePsNum={activePsNum}
        onSetPs={setPs}
        viewedCount={viewedCount}
        filesCount={filesCount}
        pct={pct}
        mode={mode}
        onSetView={setView}
        overlay={overlay}
        onSetOverlay={setOverlay}
        overlayCounts={overlayCounts}
        githubThreadsData={reviewGithubThreadsQ.data}
        ctxDial={ctxDial}
        onSetCtx={setCtx}
        noiseMode={noiseMode}
        onSetNoiseMode={setNoiseMode}
        noiseStats={noiseStats}
        isMobile={isMobile}
        mapOpen={mapOpen}
        mapParamOpen={mapParamOpen}
        onSetMapOpen={setMapOpen}
        draftsOpen={draftsOpen}
        onSetDraftsOpen={setDraftsOpen}
        drafts={drafts}
        tourOn={tourOn}
        tourStops={tourStops}
        tourStepIdx={tourStepIdx}
        onStartTour={startTour}
        onExitTour={exitTour}
        hasPrev={hasPrev}
        hasNext={hasNext}
        cursorIdx={cursorIdx}
        onGoFile={goFile}
        filesOpen={filesOpen}
        onSetFilesOpen={setFilesOpen}
        jump={jump}
        onSetJump={setJump}
        jumpHits={jumpHits}
        paths={paths}
        onSetHelpOpen={setHelpOpen}
      />
      <ReviewDiffCenter
        repo={repo}
        id={id}
        psQuery={psQuery}
        baseSha={baseSha}
        tipSha={tipSha}
        mode={mode}
        single={single}
        focusPath={focusPath}
        ordered={ordered}
        paths={paths}
        keys={keys}
        setKeys={setKeys}
        hunkCounts={hunkCounts}
        cursorPath={cursorPath}
        cursorIdx={cursorIdx}
        hasPrev={hasPrev}
        hasNext={hasNext}
        mapOpen={mapOpen}
        chapters={chapters}
        mapStateByPath={mapStateByPath}
        filesCount={filesCount}
        viewedCount={viewedCount}
        stops={stops}
        overlay={overlay}
        flashThreadId={flashThreadId}
        githubThreads={reviewGithubThreadsQ.data?.threads}
        onHunks={onHunks}
        onGoFile={goFile}
        onSetMapOpen={setMapOpen}
        onSetParam={setParam}
        onToggleViewed={toggleViewed}
        composeFor={composeFor}
        v2For={v2For}
        withQuery={withQuery}
        reviewDiffHref={reviewDiffHref}
        pseudoFiles={pseudoQ.data?.files ?? []}
        onPickPseudo={openPseudo}
      />
      <ReviewDiffRail
        filesOpen={filesOpen}
        onSetFilesOpen={setFilesOpen}
        ordered={ordered}
        cursorPath={cursorPath}
        fileRollup={fileRollup}
        onGoFile={goFile}
        dispositionMenuOpen={dispositionMenuOpen}
        focusThreadId={focusThreadId}
        findingsById={findingsById}
        onCloseDispositionMenu={closeDispositionMenu}
        draftsOpen={draftsOpen}
        drafts={drafts}
        publishing={publishing}
        paths={paths}
        onSetDraftsOpen={setDraftsOpen}
        onSetDrafts={setDrafts}
        removeDraft={removeDraft}
        onPublishDrafts={publishDrafts}
        onDiscardDrafts={discardDrafts}
        helpOpen={helpOpen}
        onSetHelpOpen={setHelpOpen}
      />
    </div>
  );
}
