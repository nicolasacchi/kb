import { useEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import type { ReviewDetailPr, ReviewPatchset } from "../api/types";
import { Icon } from "../components/icons";
import CockpitTabs, { type CockpitView } from "../components/reviews/CockpitTabs";
import FilesPanel, { type FileSort } from "../components/reviews/FilesPanel";
import InterdiffPanel from "../components/reviews/InterdiffPanel";
import PatchsetStrip from "../components/reviews/PatchsetStrip";
// ── PRR-U56 (§2 S4 Timeline tab + §2 S5 publish preview) ──
import PublishPreview from "../components/reviews/PublishPreview";
import ReadingOrderPanel from "../components/reviews/ReadingOrderPanel";
import ReportPanel, { hasReviewReport } from "../components/reviews/ReportPanel";
import ReviewHeader from "../components/reviews/ReviewHeader";
import ReviewMapPanel from "../components/reviews/ReviewMapPanel";
import ReviewSidePanel from "../components/reviews/ReviewSidePanel";
import TimelinePanel from "../components/reviews/TimelinePanel";
import { useIsMobile } from "../hooks/useIsMobile";
import { useReviewComments } from "../hooks/useReviewComments";
import {
  useGithubThreads,
  useReview,
  useReviewFiles,
  useReviewInterdiff,
  useReviewMap,
  useReviewReadingOrder,
  useReviewReport,
  useReviewTimeline,
} from "../hooks/useReviews";
import { readerUrl } from "../lib/breadcrumbs";
import { parseReviewPs, parseReviewTab } from "../lib/codeUrl";
import { indexThreads } from "../lib/reviewComments";
import { toast } from "../lib/toast";
import "../styles/reviews.css";
import "../styles/review-room.css";
import "../styles/recipes.css";
import "../styles/stacks.css";
import "../styles/history.css";

/// V3.R2 — `/r/{repo}/~reviews/{id}` review cockpit: patchset timeline,
/// files table with inline diffs + viewed tracking, annotations side
/// panel, minimal intent (session) card, and loopback-gated mutations.
export default function ReviewDetail() {
  const { repo = "", id: idParam = "" } = useParams<{ repo: string; id: string }>();
  const id = Number(idParam);
  const idOk = Number.isFinite(id) && id > 0;
  const navigate = useNavigate();

  // ── PRR-U56 (§2 S5) — the publish-preview modal, entered from the side
  // panel's "Preview round →" OR the `?publish=1` deep link (design doc §5's
  // grammar). `searchParams` is the source of truth for the INITIAL open
  // state only — subsequent opens/closes are plain component state, mirrored
  // back onto the URL with `replace: true` so it never grows the history
  // stack (same posture `?thread=`/`?finding=` deep-links use elsewhere).
  const [searchParams, setSearchParams] = useSearchParams();
  const [publishPreviewOpen, setPublishPreviewOpen] = useState(
    () => searchParams.get("publish") === "1",
  );
  function openPublishPreview() {
    setPublishPreviewOpen(true);
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.set("publish", "1");
        return next;
      },
      { replace: true },
    );
  }
  function closePublishPreview() {
    setPublishPreviewOpen(false);
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        next.delete("publish");
        return next;
      },
      { replace: true },
    );
  }

  const reviewQ = useReview(repo, idOk ? id : undefined);
  const review = reviewQ.data;
  const patchsets = review?.patchsets ?? [];
  const latestPs = patchsets.length > 0 ? patchsets[patchsets.length - 1].ps_number : null;

  // V70-A3S — the selected patchset lives in `?ps=` (was local state):
  // deep-linkable, restorable on reload, Back-able. Derived PURELY from
  // `searchParams` every render (same posture Wave E's `pane2Loc` already
  // takes in Reader.tsx) — `psSel` itself carries no independent state.
  const psSel: number | "latest" = parseReviewPs(searchParams.get("ps")) ?? "latest";
  function setPsSel(n: number | "latest") {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (n === "latest") next.delete("ps");
        else next.set("ps", String(n));
        return next;
      },
      // View state, not navigation — replace, same rule `tab` below follows
      // (root CLAUDE.md's #23 posture: URL-as-cache, not a history entry
      // per click).
      { replace: true },
    );
  }
  const activePsNum =
    psSel === "latest" ? latestPs : patchsets.some((p) => p.ps_number === psSel) ? psSel : latestPs;
  const psQuery = activePsNum == null ? "latest" : String(activePsNum);

  const [compareMode, setCompareMode] = useState(false);
  const [fromPs, setFromPs] = useState<number | null>(null);
  const [toPs, setToPs] = useState<number | null>(null);

  const filesQ = useReviewFiles(repo, idOk ? id : undefined, psQuery, !compareMode);
  const interdiffFrom =
    fromPs != null && toPs != null ? Math.min(fromPs, toPs) : undefined;
  const interdiffTo =
    fromPs != null && toPs != null ? Math.max(fromPs, toPs) : undefined;
  const interdiffQ = useReviewInterdiff(
    repo,
    idOk ? id : undefined,
    interdiffFrom,
    interdiffTo,
    compareMode && interdiffFrom != null && interdiffTo != null && interdiffFrom !== interdiffTo,
  );

  const [expanded, setExpanded] = useState<string | null>(null);
  const [pathFilter, setPathFilter] = useState("");
  const [fileSort, setFileSort] = useState<FileSort>("diff");
  // V70-A3S — the cockpit tab lives in `?tab=` (was local state): deep-
  // linkable (`~reviews/:id?tab=map` lands on the Map tab), restorable on
  // reload, Back-able. Same derive-from-`searchParams`-every-render posture
  // as `psSel` above — no independent state, `?tab=` IS the state.
  const cockpitView: CockpitView = parseReviewTab(searchParams.get("tab")) ?? "files";
  function setCockpitView(view: CockpitView) {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (view === "files") next.delete("tab");
        else next.set("tab", view);
        return next;
      },
      // View state, not navigation — replace (see `setPsSel`'s own note).
      { replace: true },
    );
  }
  const mapQ = useReviewMap(repo, idOk ? id : undefined, cockpitView === "map");
  const orderQ = useReviewReadingOrder(repo, idOk ? id : undefined, cockpitView === "order");
  const mapAvailable = mapQ.data !== null || (!mapQ.isFetched && !mapQ.isError);
  const orderAvailable = orderQ.data !== null || (!orderQ.isFetched && !orderQ.isError);
  const [tourIdx, setTourIdx] = useState(0);

  // ── PRR-U56 (§2 S4) — Timeline tab. Same fetch-on-open + 404→null
  // degrade convention as Map/Reading-order above.
  const timelineQ = useReviewTimeline(repo, idOk ? id : undefined, cockpitView === "timeline");
  const timelineAvailable = timelineQ.data !== null || (!timelineQ.isFetched && !timelineQ.isError);

  // ── PRR-F (design-addendum-2.md §A) — GitHub threads, fetched once the
  // review's `pr_number` is known (the hook itself stays disabled until
  // then). Shared across the Timeline tab (interleave) and the side panel's
  // "GitHub (N)" filter chip — one fetch, two consumers.
  const prNumberForThreads = (review as ReviewDetailPr | undefined)?.pr_number;
  const githubThreadsQ = useGithubThreads(repo, idOk ? id : undefined, prNumberForThreads);

  // PRR-U2 — Report tab. `reportQ` is fetched unconditionally (cheap, bearer,
  // prefix-invalidated with the rest of the review surface) so the header's
  // verdict dialectic strip + the tab's own availability both read it
  // without a second round-trip once the tab is opened.
  const reportQ = useReviewReport(repo, idOk ? id : undefined);
  const reportAvailable = hasReviewReport(reportQ.data);
  const [tabDefaulted, setTabDefaulted] = useState(false);
  useEffect(() => {
    if (tabDefaulted || !reportQ.isFetched) return;
    // V70-A3S — an explicit `?tab=` deep link (e.g. a `~reviews/:id?tab=map`
    // bookmark) wins over the "default to Report" rule: this auto-default
    // only fires when the URL didn't already name a tab.
    if (reportAvailable && searchParams.get("tab") === null) setCockpitView("report");
    setTabDefaulted(true);
    // Only ever runs ONCE per mount, right after the report query first
    // settles (success OR the `{report:null}` no-report case) — the
    // `tabDefaulted` guard above means this never stomps a later manual tab
    // click (§2 S2: "default tab = Report when present, else Files").
  }, [reportQ.isFetched, reportAvailable, tabDefaulted, searchParams]);
  const isMobile = useIsMobile();
  const [sheetOpen, setSheetOpen] = useState(false);
  const commentsQ = useReviewComments(repo, idOk ? id : undefined, psQuery, true);
  const openThreadCount = useMemo(
    () => (commentsQ.data ? indexThreads(commentsQ.data).rollup.open : 0),
    [commentsQ.data],
  );

  useEffect(() => {
    if (!sheetOpen) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      e.preventDefault();
      setSheetOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sheetOpen]);

  useEffect(() => {
    if (cockpitView === "map" && mapQ.isFetched && mapQ.data === null) {
      toast.err("Review map is not available on this server version.");
      setCockpitView("files");
    }
    if (cockpitView === "order" && orderQ.isFetched && orderQ.data === null) {
      toast.err("Reading order is not available on this server version.");
      setCockpitView("files");
    }
    if (cockpitView === "timeline" && timelineQ.isFetched && timelineQ.data === null) {
      toast.err("Timeline is not available on this server version.");
      setCockpitView("files");
    }
  }, [
    cockpitView,
    mapQ.isFetched,
    mapQ.data,
    orderQ.isFetched,
    orderQ.data,
    timelineQ.isFetched,
    timelineQ.data,
  ]);

  const files = filesQ.data?.files ?? [];
  const activePs: ReviewPatchset | undefined =
    activePsNum != null ? patchsets.find((p) => p.ps_number === activePsNum) : undefined;
  const baseSha = filesQ.data?.base_sha ?? activePs?.base_sha_full;
  const tipSha = filesQ.data?.tip_sha ?? activePs?.tip_sha_full;

  function tipOf(n: number | undefined): string | undefined {
    if (n == null) return undefined;
    const p = patchsets.find((ps) => ps.ps_number === n);
    return p?.tip_sha_full || p?.tip_sha || undefined;
  }

  function onComparePick(n: number) {
    if (fromPs == null) {
      setFromPs(n);
      setToPs(null);
    } else if (toPs == null) {
      if (n === fromPs) setFromPs(null);
      else setToPs(n);
    } else {
      setFromPs(n);
      setToPs(null);
    }
  }

  function openFile(path: string) {
    setExpanded((cur) => (cur === path ? null : path));
    if (compareMode) setCompareMode(false);
  }

  if (!idOk) {
    return <div className="kbc-reader__hint kbc-reader__hint--error">Invalid review id.</div>;
  }
  if (reviewQ.isLoading) {
    return <div className="kbc-reader__hint">Loading review…</div>;
  }
  if (reviewQ.error || !review) {
    return (
      <div className="kbc-reader__hint kbc-reader__hint--error">
        {(reviewQ.error as Error | undefined)?.message ?? "Review not found"}
      </div>
    );
  }

  const interdiffReady =
    compareMode && interdiffFrom != null && interdiffTo != null && interdiffFrom !== interdiffTo;

  function openFileAndCloseSheet(path: string) {
    openFile(path);
    setSheetOpen(false);
  }

  return (
    <div
      className={"kbc-review" + (isMobile && sheetOpen ? " kbc-review--sheet-open" : "")}
      id="main"
      data-kbc-review={id}
    >
      <ReviewHeader
        repo={repo}
        id={id}
        review={review}
        activePs={activePs}
        files={files}
        report={hasReviewReport(reportQ.data) ? reportQ.data : undefined}
      />
      <button
        type="button"
        className="kbc-review-sheet-toggle"
        onClick={() => setSheetOpen((v) => !v)}
        aria-label="review threads"
        aria-controls="kbc-review-sheet"
        aria-expanded={sheetOpen}
        data-kbc-review-sheet-toggle
      >
        <Icon.Panel />
        <span className="kbc-review-sheet-toggle__badge" data-kbc-review-sheet-toggle-badge>
          {openThreadCount}
        </span>
      </button>
      <PatchsetStrip
        patchsets={patchsets}
        compareMode={compareMode}
        fromPs={fromPs}
        toPs={toPs}
        activePsNum={activePsNum}
        interdiffFrom={interdiffFrom}
        interdiffTo={interdiffTo}
        onSelectPs={(n) => {
          setPsSel(n);
          setExpanded(null);
        }}
        onComparePick={onComparePick}
        onToggleCompare={() => {
          setCompareMode((v) => {
            if (v) {
              setFromPs(null);
              setToPs(null);
            }
            return !v;
          });
          setExpanded(null);
        }}
      />
      <CockpitTabs
        compareMode={compareMode}
        reportAvailable={reportAvailable}
        mapAvailable={mapAvailable}
        orderAvailable={orderAvailable}
        timelineAvailable={timelineAvailable}
        cockpitView={cockpitView}
        onSelect={(view) => {
          setCockpitView(view);
          if (view === "order") setTourIdx(0);
        }}
      />
      <div className="kbc-review__body">
        <div className="kbc-review__main">
          {!compareMode && cockpitView === "report" ? (
            <ReportPanel
              repo={repo}
              review={review as ReviewDetailPr}
              ps={psQuery}
              onOpenFilesTab={() => setCockpitView("files")}
            />
          ) : !compareMode && cockpitView === "map" ? (
            <ReviewMapPanel
              repo={repo}
              loading={mapQ.isLoading}
              error={mapQ.error as Error | null}
              data={mapQ.data}
              onOpenFile={(path) => navigate(readerUrl(repo, path))}
            />
          ) : !compareMode && cockpitView === "order" ? (
            <ReadingOrderPanel
              repo={repo}
              reviewId={id}
              loading={orderQ.isLoading}
              error={orderQ.error as Error | null}
              data={orderQ.data}
              tourIdx={tourIdx}
              setTourIdx={setTourIdx}
              onOpenFile={(path) => navigate(readerUrl(repo, path))}
            />
          ) : !compareMode && cockpitView === "timeline" ? (
            <TimelinePanel
              repo={repo}
              reviewId={id}
              loading={timelineQ.isLoading}
              error={timelineQ.error as Error | null}
              data={timelineQ.data}
              githubThreads={githubThreadsQ.data}
            />
          ) : compareMode && interdiffReady ? (
            <InterdiffPanel
              repo={repo}
              loading={interdiffQ.isLoading}
              error={interdiffQ.error as Error | null}
              data={interdiffQ.data}
              fromTipSha={tipOf(interdiffFrom)}
              toTipSha={tipOf(interdiffTo)}
            />
          ) : compareMode ? (
            <div className="kbc-reader__hint">Select two patchsets to compare.</div>
          ) : (
            <FilesPanel
              repo={repo}
              reviewId={id}
              files={files}
              loading={filesQ.isLoading}
              error={(filesQ.error as Error | null) ?? null}
              expanded={expanded}
              onOpenFile={openFile}
              pathFilter={pathFilter}
              onPathFilter={setPathFilter}
              fileSort={fileSort}
              onFileSort={setFileSort}
              baseSha={baseSha}
              tipSha={tipSha}
              ps={psQuery}
            />
          )}
        </div>
        <ReviewSidePanel
          repo={repo}
          id={id}
          ps={psQuery}
          sessionId={review.session_id ?? null}
          onOpenFile={openFileAndCloseSheet}
          asSheet={isMobile}
          onMobileClose={isMobile ? () => setSheetOpen(false) : undefined}
          onOpenPublishPreview={openPublishPreview}
          prNumber={prNumberForThreads}
        />
      </div>
      {isMobile && sheetOpen && (
        <div
          className="kbc-sheet-scrim is-open"
          onClick={() => setSheetOpen(false)}
          aria-hidden
          data-kbc-review-sheet-scrim
        />
      )}
      {/* ── PRR-U56 (§2 S5) — the publish preview modal, entered from the
          side panel OR `?publish=1`. */}
      {publishPreviewOpen && (
        <PublishPreview
          repo={repo}
          reviewId={id}
          review={review as ReviewDetailPr}
          onClose={closePublishPreview}
        />
      )}
    </div>
  );
}
