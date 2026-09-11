import { useEffect, useLayoutEffect, useMemo, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { Group, Panel, Separator, usePanelRef, type Layout } from "react-resizable-panels";
import type { FindingSeverity, ReviewDetailPr, ReviewPatchset } from "../api/types";
import { Icon } from "../components/icons";
import CockpitTabs, { type CockpitView } from "../components/reviews/CockpitTabs";
// ── V73-K2b (kbc-review/1, design D9/D9-a) — the Document tab ──
import DocPanel from "../components/reviews/DocPanel";
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
import type { FindingSeverityFilter } from "../components/reviews/ReviewThreadsCard";
import TimelinePanel from "../components/reviews/TimelinePanel";
import { useCommandHandlers, useCommandScope } from "../commands/CommandRoot";
import { useIsMobile } from "../hooks/useIsMobile";
import { useReviewComments } from "../hooks/useReviewComments";
import {
  useClaims,
  useReview,
  useReviewDoc,
  useReviewDocLint,
  useReviewFiles,
  useReviewInterdiff,
  useReviewMap,
  useReviewReadingOrder,
  useReviewReport,
  useReviewTimeline,
} from "../hooks/useReviews";
import { readerUrl } from "../lib/breadcrumbs";
import { parseDocCardsMode, parseReviewPs, parseReviewTab } from "../lib/codeUrl";
import { cardList } from "../lib/reviewDoc";
import { indexThreads } from "../lib/reviewComments";
// ── V76-R2a — the Room's rail geometry + density. The rail's truth is the
// pure reducer; react-resizable-panels is the mechanism; keyboard resize
// goes through the DESK's resize submode (via `railKeyResize`) — no second
// resizer. ──
import {
  REVIEW_RAIL_DEFAULT_WIDTH,
  loadReviewRail,
  railKeyResize,
  railWidthFromLayout,
  resetRail,
  roomLayout,
  saveReviewRail,
  toggleRail,
  type ReviewRailState,
} from "../lib/reviewRail";
import {
  ROOM_DENSITY_STORAGE_KEY,
  nextRoomDensity,
  parseRoomDensity,
  type RoomDensity,
} from "../lib/reviewRoom";
import { RESIZE_SUBMODE_HINT } from "../desk/resizeSubmode";
import { toast } from "../lib/toast";
import "../styles/reviews.css";
import "../styles/review-room.css";
import "../styles/recipes.css";
import "../styles/stacks.css";
import "../styles/history.css";
import "../styles/review-doc.css";

/// Escape a value for use inside an attribute selector. Same fallback shape
/// `routes/reviewDiff/helpers.ts`'s `cssAttr` carries, for the same reason:
/// `CSS.escape` is absent in the node test environment and on old engines.
function cssEscape(value: string): string {
  if (typeof CSS !== "undefined" && typeof CSS.escape === "function") return CSS.escape(value);
  return value.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

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

  // ── V73-K2b (kbc-review/1) — the Document tab. Same fetch-on-open +
  // 404→null degrade convention as Map/Order/Timeline above: a review with
  // no composed document hides the tab rather than showing empty chrome.
  // The read always asks for `?resolve=true` — a document without live cards
  // is the prose the CLI already prints (`useReviewDoc`'s own note).
  const docQ = useReviewDoc(repo, idOk ? id : undefined, psQuery, cockpitView === "doc");
  const docLintQ = useReviewDocLint(repo, idOk ? id : undefined, psQuery, cockpitView === "doc");
  const docAvailable = docQ.data !== null || (!docQ.isFetched && !docQ.isError);
  // `?cards=folded` — the tab's one knob, and it lives in the URL for the
  // same reason every diff-v2 knob does: a reload reproduces the view and
  // there is no parallel store to drift.
  const cardsFolded = parseDocCardsMode(searchParams.get("cards")) === "folded";
  function setCardsFolded(folded: boolean) {
    setSearchParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        if (folded) next.set("cards", "folded");
        else next.delete("cards");
        return next;
      },
      { replace: true },
    );
  }
  // The focused ref card. Browser-local by design: it is a cursor, not a
  // place — `?cards=` reproduces the LAYOUT, and a card permalink is the
  // card's own address (`RefCard`'s link), not a highlight in this tab.
  const [focusedRef, setFocusedRef] = useState<string | null>(null);
  const docCards = useMemo(() => cardList(docQ.data), [docQ.data]);

  // ── PRR-F (design-addendum-2.md §A) — the PR number the side panel's
  // publish-preview + `TimelinePanel`'s `github=` default both key off.
  // V73-K2c retired this route's own `useGithubThreads` call: the Timeline
  // tab's client-side GitHub-thread interleave is gone now that
  // `review-timeline/2`'s own `github` lane natively carries
  // `github_comment` events (`TimelinePanel.tsx`'s own doc) — merging the
  // two would double the rows.
  const prNumberForThreads = (review as ReviewDetailPr | undefined)?.pr_number;

  // PRR-U2 — Report tab. `reportQ` is fetched unconditionally (cheap, bearer,
  // prefix-invalidated with the rest of the review surface) so the header's
  // verdict dialectic strip + the tab's own availability both read it
  // without a second round-trip once the tab is opened.
  const reportQ = useReviewReport(repo, idOk ? id : undefined);
  const reportAvailable = hasReviewReport(reportQ.data);

  // V73-K2c (kbc-claim/1, design D18) — the claim register's ONE fetch,
  // shared by the Document tab (`DocPanel`) and the Report tab's
  // beside-findings register (`ReportPanel`), same "one fetch, two
  // consumers" precedent `githubThreadsQ` above establishes. Cheap bearer
  // read, fetched unconditionally like `reportQ`.
  const claimsQ = useClaims(idOk ? { repo, review: id } : undefined, idOk);
  const claims = claimsQ.data?.claims ?? [];
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

  // ── V76-R2a — the findings rail as a RESIZABLE DOCK ─────────────────────
  //
  // The Desk's three rules, restated for the Room (see `lib/reviewRail.ts`'s
  // header): the pure reducer is the truth, `react-resizable-panels` is the
  // mechanism (no `autoSaveId`), and keyboard resize on the focused
  // separator goes through the DESK's `resizeSubmodeKey` (`focus: "rail"`)
  // — this route does not write a second resizer.
  const [rail, setRail] = useState<ReviewRailState>(() => loadReviewRail());
  const railPanel = usePanelRef();
  useEffect(() => {
    saveReviewRail(rail);
  }, [rail]);
  // Collapse/expand + imperative width live OUTSIDE the layout write-back,
  // exactly as `Desk.tsx` does it: `defaultLayout` is read at mount only.
  useLayoutEffect(() => {
    if (isMobile) return;
    if (rail.collapsed) railPanel.current?.collapse();
    else railPanel.current?.expand();
  }, [isMobile, rail.collapsed, railPanel]);
  useLayoutEffect(() => {
    if (isMobile || rail.collapsed) return;
    // The `%` suffix is load-bearing: the library reads a bare NUMBER as
    // PIXELS (Desk.tsx's own warning beside `defaultSize`).
    railPanel.current?.resize(`${rail.width}%`);
  }, [isMobile, rail.collapsed, rail.width, railPanel]);
  const onRoomLayout = (layout: Layout, meta: { isUserInteraction: boolean }) => {
    if (!meta.isUserInteraction) return;
    const w = railWidthFromLayout(layout);
    if (w == null) return;
    setRail((cur) => (cur.width === w ? cur : { ...cur, width: w }));
  };
  /// The focused separator's keydown, routed through the Desk's resize
  /// submode. Only keys the submode claims are stopped here (the
  /// focused-panel rule: stop only the keys you handle). `rail` is read
  /// from the closure (this handler is rebuilt every render, so it is never
  /// stale) — the updater form would run side effects inside the reducer,
  /// which StrictMode may invoke twice.
  function onRailSepKey(e: React.KeyboardEvent) {
    const result = railKeyResize(rail, e.key);
    if (!result.handled) return;
    e.preventDefault();
    e.stopPropagation();
    if (result.command.t === "exit") (e.target as HTMLElement).blur();
    if (result.command.t === "hint") toast.ok(RESIZE_SUBMODE_HINT);
    if (result.state !== rail) setRail(result.state);
  }

  // ── V76-R2a — the Room density toggle. localStorage is the home;
  // `?density=` only mirrors it (the `lib/branchViews.ts` precedent). ──
  const [densityState, setDensityState] = useState<RoomDensity>(() =>
    parseRoomDensity(
      typeof localStorage !== "undefined" ? localStorage.getItem(ROOM_DENSITY_STORAGE_KEY) : null,
    ),
  );
  // An explicit `?density=` wins (a shared link carries the density);
  // absent, the mirrored/persisted state. Same posture as `?ps=` above.
  const densityParam = searchParams.get("density");
  const density: RoomDensity = densityParam ? parseRoomDensity(densityParam) : densityState;
  function setDensity(next: RoomDensity) {
    setDensityState(next);
    try {
      localStorage.setItem(ROOM_DENSITY_STORAGE_KEY, next);
    } catch {
      // private-mode refusal — the toggle still works for this session.
    }
    setSearchParams(
      (prev) => {
        const p = new URLSearchParams(prev);
        if (next === "compact") p.set("density", "compact");
        else p.delete("density");
        return p;
      },
      { replace: true },
    );
  }

  // ── V76-R2a — the rail's findings severity filter, LIFTED so the Report
  // hero's count chips and the rail's own filter row are ONE state. ──
  const [findingSevFilter, setFindingSevFilter] = useState<FindingSeverityFilter>("all");
  function filterRailTo(sev: FindingSeverity) {
    setFindingSevFilter(sev);
    if (isMobile) setSheetOpen(true);
    else setRail((cur) => (cur.collapsed ? { ...cur, collapsed: false } : cur));
    requestAnimationFrame(() => {
      document
        .querySelector("[data-kbc-review-findings]")
        ?.scrollIntoView({ block: "start", behavior: "smooth" });
    });
  }

  /// V76-R2a — `review.jump.*`: open the Report tab (a no-op when already
  /// there), then scroll the named section decorator into view.
  function jumpToRoomSection(kind: "summary" | "findings" | "praise" | "verdict") {
    if (!reportAvailable) return;
    if (cockpitView !== "report") setCockpitView("report");
    requestAnimationFrame(() => {
      requestAnimationFrame(() => {
        document
          .querySelector(`[data-kbc-room-section="${kind}"]`)
          ?.scrollIntoView({ block: "start", behavior: "smooth" });
      });
    });
  }

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
    if (cockpitView === "doc" && docQ.isFetched && docQ.data === null) {
      // Two causes, one honest sentence: this review has no composed
      // document, or the daemon predates the surface. The tab never
      // distinguishes them because the 404 does not either.
      toast.err("This review has no kbc-review/1 document (compose one with `kb-code review compose`).");
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
    docQ.isFetched,
    docQ.data,
  ]);

  // ── V73-K2b — the cockpit's KEYS, as kbc-cmd/1 handlers.
  //
  // This is the first surface to publish `scope: "review"`. The five
  // `review.tab.*` rows have existed since v7.0 and dispatched NOWHERE —
  // shipping `6` (Document) beside a dead `1` would be exactly the
  // silently-dead-row failure web-code/CLAUDE.md's keyboard section exists
  // to prevent, so all six are registered here together.
  //
  // Every row is `dispatch: "surface"` and none carries a `vim_kind`: this
  // route mounts no `CodeView`, so `shouldWithholdFromBuffer` (which
  // resolves in `"reader"` scope) never sees any of them — the same reason
  // the diff-scope rows are safe. `] r`/`[ r` follow the `] p`/`] u`/`] d`
  // precedent exactly: `[`/`]` is a MIXED prefix and a vim arm would fire
  // the step twice.
  useCommandScope("review", {
    "review.open": true,
    "help.open": false,
  });
  function selectTab(view: CockpitView, available: boolean) {
    // A tab that is not available is a NO-OP, never a toast and never a
    // navigation to empty chrome — the key means "show me that tab", and
    // there is no tab to show.
    if (!available) return;
    setCockpitView(view);
    if (view === "order") setTourIdx(0);
  }
  function stepCard(delta: 1 | -1) {
    if (docCards.length === 0) return;
    const at = focusedRef === null ? -1 : docCards.findIndex((c) => c.ref === focusedRef);
    const next = at === -1 ? (delta === 1 ? 0 : docCards.length - 1) : at + delta;
    const card = docCards[(next + docCards.length) % docCards.length];
    if (card) setFocusedRef(card.ref);
  }
  useCommandHandlers({
    "review.tab.report": () => selectTab("report", reportAvailable),
    "review.tab.files": () => selectTab("files", true),
    "review.tab.map": () => selectTab("map", mapAvailable),
    "review.tab.order": () => selectTab("order", orderAvailable),
    "review.tab.timeline": () => selectTab("timeline", timelineAvailable),
    "review.tab.doc": () => selectTab("doc", docAvailable),
    "doc.cards-fold": () => {
      if (cockpitView !== "doc") return;
      setCardsFolded(!cardsFolded);
    },
    "doc.card-next": () => {
      if (cockpitView !== "doc") return;
      stepCard(1);
    },
    "doc.card-prev": () => {
      if (cockpitView !== "doc") return;
      stepCard(-1);
    },
    "doc.card-open": () => {
      if (cockpitView !== "doc" || focusedRef === null) return;
      const el = document.querySelector(`[data-kbc-refcard-link="${cssEscape(focusedRef)}"]`);
      // An orphan and an inert card have NO link — by construction, not by
      // omission. Nothing to open is the honest outcome, not a fallback
      // navigation to somewhere adjacent.
      if (el instanceof HTMLElement) el.click();
    },
    "doc.compose-copy": () => {
      if (cockpitView !== "doc") return;
      const el = document.querySelector("[data-kbc-doc-compose-copy]");
      if (el instanceof HTMLElement) el.click();
    },
    // V73-K2c — the claim register is mounted on BOTH the Document and
    // Report tabs (never both at once, since `cockpitView` renders exactly
    // one), so this is a plain DOM-click delegation with no tab gate, same
    // pattern `doc.compose-copy` above already uses.
    "review.claims-toggle": () => {
      const el = document.querySelector("[data-kbc-claims-toggle]");
      if (el instanceof HTMLElement) el.click();
    },
    // V73-K2c — the Timeline tab's own lane visibility + GitHub toggle.
    // Both are plain DOM-click delegation onto `TimelinePanel.tsx`'s own
    // buttons, same pattern `doc.compose-copy`/`review.claims-toggle`
    // already use — gated on the tab so the keys are inert elsewhere.
    "review.timeline.lane-cycle": () => {
      if (cockpitView !== "timeline") return;
      const el = document.querySelector("[data-kbc-timeline-lane-cycle]");
      if (el instanceof HTMLElement) el.click();
    },
    "review.timeline.github-toggle": () => {
      if (cockpitView !== "timeline") return;
      const el = document.querySelector("[data-kbc-timeline-github-toggle]");
      if (el instanceof HTMLElement) el.click();
    },
    // ── V76-R2a — the Room's rail + density + section jumps. Same
    // `dispatch: "surface"` / no-`vim_kind` posture as every row above.
    // `Space r` was NOT free (it is `doc.cards-fold` in this very scope),
    // so the rail toggle is `Space i`; all seven keys are whole-registry
    // unique — `commands/reviewRoom.test.ts`'s prefix scan is the gate the
    // doctor structurally cannot be for leader chords. ──
    "review.rail.toggle": () => {
      if (isMobile) setSheetOpen((v) => !v);
      else setRail((cur) => toggleRail(cur));
    },
    "review.rail.reset": () => setRail(resetRail),
    "review.density-toggle": () => setDensity(nextRoomDensity(density)),
    "review.jump.summary": () => jumpToRoomSection("summary"),
    "review.jump.findings": () => jumpToRoomSection("findings"),
    "review.jump.praise": () => jumpToRoomSection("praise"),
    "review.jump.verdict": () => jumpToRoomSection("verdict"),
  });

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

  // V76-R2a — the center content and the rail are rendered in TWO layout
  // shells below (the mobile single-sheet grid vs. the desktop resizable
  // dock), so both are bound ONCE here.
  const mainContent = !compareMode && cockpitView === "report" ? (
    <ReportPanel
      repo={repo}
      review={review as ReviewDetailPr}
      ps={psQuery}
      onOpenFilesTab={() => setCockpitView("files")}
      claims={claims}
      onFilterFindings={filterRailTo}
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
    <TimelinePanel repo={repo} reviewId={id} prBound={prNumberForThreads != null} />
  ) : !compareMode && cockpitView === "doc" ? (
    docQ.isLoading ? (
      <div className="kbc-reader__hint">Loading the review document…</div>
    ) : docQ.error ? (
      <div className="kbc-reader__hint kbc-reader__hint--error">
        {(docQ.error as Error).message}
      </div>
    ) : docQ.data ? (
      <DocPanel
        repo={repo}
        id={id}
        doc={docQ.data}
        lint={docLintQ.data}
        lintLoading={docLintQ.isLoading}
        cardsFolded={cardsFolded}
        onSetCardsFolded={setCardsFolded}
        focusedRef={focusedRef}
        claims={claims}
      />
    ) : (
      <div className="kbc-reader__hint">
        This review has no kbc-review/1 document yet.
      </div>
    )
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
  );

  return (
    <div
      className={
        "kbc-review" +
        (isMobile && sheetOpen ? " kbc-review--sheet-open" : "") +
        (density === "compact" ? " kbc-review--compact" : "")
      }
      id="main"
      data-kbc-review={id}
      data-kbc-room-density={density}
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
        docAvailable={docAvailable}
        cockpitView={cockpitView}
        onSelect={(view) => {
          setCockpitView(view);
          if (view === "order") setTourIdx(0);
        }}
      />
      {isMobile ? (
        // ── ≤860px: the EXISTING single-sheet behaviour, untouched — the
        // rail is the one bottom sheet, never a second surface (root
        // CLAUDE.md invariant #30). ──
        <div className="kbc-review__body">
          <div className="kbc-review__main">{mainContent}</div>
          <ReviewSidePanel
            repo={repo}
            id={id}
            ps={psQuery}
            sessionId={review.session_id ?? null}
            onOpenFile={openFileAndCloseSheet}
            asSheet
            onMobileClose={() => setSheetOpen(false)}
            onOpenPublishPreview={openPublishPreview}
            prNumber={prNumberForThreads}
            docCards={cockpitView === "doc" ? docCards : undefined}
            focusedRef={focusedRef}
            onFocusRef={setFocusedRef}
            findingSeverityFilter={findingSevFilter}
            onFindingSeverityFilter={setFindingSevFilter}
          />
        </div>
      ) : (
        // ── V76-R2a — desktop: the Room fills the viewport; the findings
        // rail is a resizable dock. `react-resizable-panels` is the
        // MECHANISM (the Desk's own library, same three rules — see
        // `lib/reviewRail.ts`'s header); `lib/reviewRail.ts`'s reducer is
        // the truth. ──
        <>
          <Group
            id="room-cols"
            className="kbc-review__body kbc-review__body--room"
            orientation="horizontal"
            // See Desk.tsx's header, rule 2 — the Lumino/Chromium trap.
            disableCursor
            defaultLayout={roomLayout(rail.width)}
            onLayoutChanged={onRoomLayout}
            resizeTargetMinimumSize={{ coarse: 24, fine: 8 }}
          >
            <Panel id="room-main" className="kbc-room__panel" minSize="35%" style={{ overflow: "hidden" }}>
              <div className="kbc-review__main">{mainContent}</div>
            </Panel>
            <Separator
              id="sep-room-rail"
              className="kbc-desk__sep kbc-desk__sep--v kbc-room__sep"
              data-kbc-room-sep
              onKeyDown={onRailSepKey}
            />
            <Panel
              id="room-rail"
              className="kbc-room__panel"
              panelRef={railPanel}
              collapsible
              collapsedSize={0}
              minSize="220px"
              // The DEFAULT width, not the persisted one — this is what a
              // separator double-click resets to (Desk.tsx's rule 3), the
              // same value `Space I` restores.
              defaultSize={`${REVIEW_RAIL_DEFAULT_WIDTH}%`}
              style={{ overflow: "hidden" }}
            >
              <ReviewSidePanel
                repo={repo}
                id={id}
                ps={psQuery}
                sessionId={review.session_id ?? null}
                onOpenFile={openFile}
                onOpenPublishPreview={openPublishPreview}
                prNumber={prNumberForThreads}
                docCards={cockpitView === "doc" ? docCards : undefined}
                focusedRef={focusedRef}
                onFocusRef={setFocusedRef}
                density={density}
                onToggleDensity={() => setDensity(nextRoomDensity(density))}
                onCollapseRail={() => setRail((cur) => toggleRail(cur))}
                findingSeverityFilter={findingSevFilter}
                onFindingSeverityFilter={setFindingSevFilter}
              />
            </Panel>
          </Group>
          {/* The collapsed rail's stripe: the expand affordance, OUTSIDE
              the resizable group (the Desk's own stripe posture). */}
          {rail.collapsed && (
            <button
              type="button"
              className="kbc-room__rail-strip"
              onClick={() => setRail((cur) => toggleRail(cur))}
              aria-label="show findings rail"
              title="show findings rail (Space i)"
              data-kbc-room-rail-expand
            >
              <Icon.Panel />
              <span className="kbc-room__rail-strip-lab">Findings</span>
            </button>
          )}
        </>
      )}
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
