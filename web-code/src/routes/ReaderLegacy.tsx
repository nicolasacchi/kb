// V70-A4 — the PRE-DESK reader, kept mounted behind `?shell=legacy`
// (equivalently `?desk=legacy`) for exactly one milestone.
//
// docs/research/kb-code-v7-continuum-2026-09.html §D1 ("One shell,
// migrated incrementally"): "the legacy route stays mounted behind
// `?shell=legacy` for one milestone with both paths in CI". This file is
// `routes/Reader.tsx` as it stood at `cfc47c1a`, verbatim apart from the
// component's name and this header — the fixed 260px tree, the hard
// 50/50 pane split, the fixed 220px inspector column, no drawer, no
// stripes, nothing resizable.
//
// ## What "unchanged" means here, precisely
//
// The SHELL is frozen; the CHILDREN are shared and keep evolving. This
// route renders the same `InspectorRail`, `FileTree`, `CodeView` and
// `PeekPanel` the Desk reader does, so V70-A4's rail re-cut (six source
// tabs → the design's task tabs) is visible here too. Freezing a private
// copy of the rail as well would mean two homes for one action — exactly
// what root CLAUDE.md invariant #30 forbids — and would make this an
// unmaintained fork rather than an escape hatch. The imperative
// `openTab("provenance")`/`openTab("annotations")` calls below still
// work: `InspectorRailHandle.openTab` accepts the legacy tab ids and
// translates them (`hooks/useInspectorTab.ts`'s `LEGACY_TAB_MAP`).
//
// Removal: the milestone after v7.0 deletes this file, its route branch
// in `app.tsx`, and `e2e/desk-legacy.spec.ts`.
//
import { useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useNavigate, useParams, useSearchParams } from "react-router-dom";
import { useQueryClient } from "@tanstack/react-query";
import {
  fetchDefs,
  fetchHierarchyCallees,
  fetchHierarchyCallers,
  fetchHierarchyTypes,
  fetchImpactAnalysis,
  fetchResolve,
  fetchResolveSymbol,
  fetchWhyLine,
  fetchXrefs,
} from "../api/client";
import Breadcrumbs from "../components/Breadcrumbs";
import CodeView, { type CodeViewHandle, type GotoSel } from "../components/CodeView";
import DiffView from "../components/DiffView";
import EmptyState from "../components/EmptyState";
import FileTree, { type FileTreeHandle } from "../components/FileTree";
import { Icon } from "../components/icons";
import InspectorRail, { type InspectorRailHandle } from "../components/InspectorRail";
import KeyboardHelp from "../components/KeyboardHelp";
import LineHistoryPopup from "../components/LineHistoryPopup";
import RecentLocations from "../components/RecentLocations";
import StructurePopup from "../components/StructurePopup";
import MnemonicPopup from "../components/bookmarks/MnemonicPopup";
import { FileChangedToast, HeadMovedBanner } from "../components/LiveMirrorBanners";
import HierarchyPanel from "../components/hierarchy/HierarchyPanel";
import ImpactPanel from "../components/impact/ImpactPanel";
import EgoGraph from "../components/graph/EgoGraph";
import EntityRail from "../components/entity/EntityRail";
import PeekPanel, { type PeekAnchor } from "../components/peek/PeekPanel";
import RepoStateBanner from "../components/RepoStateBanner";
import AddToSetMenu from "../components/sets/AddToSetMenu";
import BlameChip from "../components/provenance/BlameChip";
import DiagnosticsCard from "../components/provenance/DiagnosticsCard";
import FrameworkCard from "../components/provenance/FrameworkCard";
import StoryTimeline from "../components/provenance/StoryTimeline";
import WhyPanel from "../components/provenance/WhyPanel";
import HistoryPanel from "../components/history/HistoryPanel";
import CitedBy from "../components/lens/CitedBy";
import MobileDrawer from "../components/MobileDrawer";
import RefPicker from "../components/RefPicker";
import StoryPlayer from "../components/story/StoryPlayer";
import WorkingSetStrip from "../components/WorkingSetStrip";
import type { LinkifyCallbacks } from "../editor/linkify";
import type { LineMarkerSpec } from "../editor/lineGutter";
import { copyToClipboard, type LineSel, type VimReaderCallbacks, type WordPos } from "../editor/vimReader";
import { useAnnotations } from "../hooks/useAnnotations";
import { useBlame } from "../hooks/useBlame";
import { useBlameAttributions } from "../hooks/useBlameAttributions";
import {
  useBookmarks,
  useCreateBookmark,
  useDeleteBookmark,
} from "../hooks/useBookmarks";
import { useDiagnostics } from "../hooks/useDiagnostics";
import { useFile } from "../hooks/useFile";
import { useFileHistory } from "../hooks/useFileHistory";
import { useIsMobile } from "../hooks/useIsMobile";
import { useLiveMirror } from "../hooks/useLiveMirror";
import { useRepos } from "../hooks/useRepos";
import { useRepoState } from "../hooks/useRepoState";
import { useWorkingSet } from "../hooks/useWorkingSet";
import { buildAgeLineBuckets, type AgeLineInfo } from "../lib/ageHeatmap";
import { annotationGutterTitle, annotationsByLine, unresolvedCount } from "../lib/annotations";
import { buildLineDots, regionCoveringLine, type BlameDotInfo } from "../lib/blameGutter";
import { diagnosticGutterMarks, type DiagnosticGutterMark } from "../lib/diagnostics";
import {
  codeUrl,
  commitUrl,
  formatLineParam,
  parseLineParam,
  parsePane2,
  permalinkFor,
  storyUrl,
  type LineSel as PaneLineSel,
  type PaneLoc,
} from "../lib/codeUrl";
import { createCursorUrlSync, createPane2CursorUrlSync, type CursorUrlSync } from "../lib/cursorUrlSync";
import { currentHistoryIndex, historyStepTarget } from "../lib/historyStep";
import { initialLadderState, ladderReducer } from "../lib/ladderState";
import { fileChangedPaneLabel } from "../lib/liveMirror";
import {
  defRowsFrom,
  initialPeekState,
  peekReducer,
  refRowsFrom,
  resolveCandidateToRow,
  singleExactMatch,
  type HoverCard,
  type HoverProvenance,
  type PeekRow,
} from "../lib/peekState";
import {
  ancestorKeys,
  buildCalleesTree,
  buildCallersTree,
  buildTypesTree,
  calleesToNodes,
  callersToNodes,
  collectAncestorLocs,
  hierarchyReducer,
  HIERARCHY_DEPTH_CAP,
  initialHierarchyState,
  isTypeIshKind,
  typeEdgesToNodes,
  updateNode,
  type HierarchyMode,
  type HierarchyNode,
} from "../lib/hierarchyState";
import {
  impactReducer,
  initialImpactState,
  isImpactNavigable,
  type ImpactFlatRow,
} from "../lib/impactState";
import {
  layoutEgoGraph,
  type EgoLayoutInput,
  type EgoLaidOutNode,
  type EgoLayoutResult,
} from "../lib/egoGraph";
import { getRecentFiles, goBack, goForward, recordJump } from "../lib/navHistory";
import {
  loadCodeLenses,
  loadParamHints,
  loadReaderFontSize,
  loadStickyContext,
  loadWrap,
  READER_FONT_SIZE_MAX,
  READER_FONT_SIZE_MIN,
  saveCodeLenses,
  saveParamHints,
  saveReaderFontSize,
  saveStickyContext,
  saveWrap,
} from "../lib/prefs";
import { sessionUrl } from "../lib/searchLanes";
import { toast } from "../lib/toast";
import type { AttributionOut, EntryKind, LensDeclaration } from "../api/types";
import { useLenses } from "../hooks/useLenses";

const DIFF_SENTINEL = "~diff";
/// Phase C7 — like `DIFF_SENTINEL` above, a FILE-scoped sentinel trailing an
/// arbitrary path (`lib/codeUrl.ts`'s `storyUrl`), so it rides Reader's own
/// splat parsing rather than getting a dedicated `<Route>` the way the
/// repo-scoped `~commit`/`~compare`/`~branches` sentinels below do.
const STORY_SENTINEL = "~story";
/// Phase C-SPA — the repo-scoped, non-file sentinels registered as their
/// OWN routes in `app.tsx` (`~commit/:sha`, `~compare`, `~branches` — see
/// `lib/codeUrl.ts`'s header comment for why these don't ride Reader's
/// splat the way `~diff` does). React Router always prefers those static
/// routes for a well-formed URL, so this list only matters DEFENSIVELY: a
/// malformed URL missing a required param (e.g. bare `~commit`, no `:sha`)
/// falls through to this catch-all, and must render an honest hint rather
/// than treat the sentinel as a literal file/dir path.
const HISTORY_SENTINELS = [
  "~commit",
  "~compare",
  "~branches",
  "~range-diff",
  "~prs",
  "~sets",
  "~todos",
  "~hotspots",
  "~reviews",
];

function isHistorySentinel(splat: string): boolean {
  return HISTORY_SENTINELS.some((s) => splat === s || splat.startsWith(`${s}/`));
}

/// The reader's Provenance overlay is a 3-state control (Wave C) —
/// `off` (nothing fetched), `dots` (the original W4.4 gutter-dot disclosure
/// ladder), `age` (the new line-background heatmap, `lib/ageHeatmap.ts`).
type ProvenanceMode = "off" | "dots" | "age";

function isEditableTarget(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
}

/// True when the event originated inside the CM6 buffer — the vim keymap
/// (`editor/vimReader.ts`) owns EVERY key there; the window-level handler
/// below must never double-handle (`j` would move BOTH the code cursor and
/// the tree's row cursor). Matches EITHER pane's buffer (`.kbc-codeview`
/// appears once per open pane).
function isInsideBuffer(target: EventTarget | null): boolean {
  const t = target as HTMLElement | null;
  return !!t && typeof t.closest === "function" && t.closest(".kbc-codeview") !== null;
}

/// SH.C3 — the "start here" panel shown alongside the empty "select a file"
/// state (a repo landing with a populated tree but no open buffer used to
/// be a near-total void below the EmptyState card). Both cards read data
/// the app ALREADY tracks client-side — no new fetch, no new endpoint:
/// `getRecentFiles()` is the same `lib/navHistory.ts` ring the `g.`
/// `RecentLocations` popup reads, just filtered to this repo; the keyboard
/// card is purely static (a condensed pointer into the full `?` cheatsheet,
/// not a duplicate of it).
function ReaderStartCards({ repo, onOpen }: { repo: string; onOpen: (path: string) => void }) {
  const recent = useMemo(
    () => getRecentFiles().filter((f) => f.repo === repo).slice(0, 6),
    [repo],
  );
  return (
    <div className="kbc-reader-start__cards">
      {recent.length > 0 && (
        <section className="kbc-reader-start__card" aria-label="Continue where you left off">
          <h2>Continue where you left off</h2>
          <ul>
            {recent.map((f) => (
              <li key={f.path}>
                <button
                  type="button"
                  className="kbc-reader-start__row"
                  onClick={() => onOpen(f.path)}
                  data-kbc-start-recent={f.path}
                >
                  {f.path}
                </button>
              </li>
            ))}
          </ul>
        </section>
      )}
      <section className="kbc-reader-start__card" aria-label="Keyboard essentials">
        <h2>Keyboard essentials</h2>
        <dl>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>/</kbd>
            </dt>
            <dd>filter the tree</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>⌘K</kbd>
            </dt>
            <dd>search everywhere</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>g.</kbd>
            </dt>
            <dd>recent locations</dd>
          </div>
          <div className="kbc-reader-start__key">
            <dt>
              <kbd>?</kbd>
            </dt>
            <dd>full cheatsheet</dd>
          </div>
        </dl>
      </section>
    </div>
  );
}

/// The reader's home surface (W4.2, extended by W4.4/W4.5/W4.6, re-centered
/// by A3, split by Wave E): top breadcrumbs + ref picker + a "Provenance"
/// toggle, left file tree, center CM6 read-only viewer(s) (or the diff
/// view), right inspector rail (outline / provenance / annotations). Live-
/// mirror affordances (W4.5) float above the body: a dismissible file-
/// changed toast per open pane and a HEAD-moved banner.
///
/// **Focus model (A3, extended by Wave E)**: a BUFFER is the primary focus
/// surface — but now there can be TWO (`pane1`/`pane2`), and `focusedPane`
/// (1 | 2, plain UI state — not itself part of the shareable URL, same
/// footing as `treeVisible`) tracks which one is live. `focusedPane` is kept
/// in sync with REAL DOM focus via each pane's own `onFocus` (React's
/// synthetic focus event bubbles from the CM6 content div through the
/// wrapper), so both a mouse click into a pane and a programmatic
/// `Ctrl-w h/l` `.focus()` call converge on the same source of truth — no
/// separate bookkeeping needed in the pane-focus handler itself. Every
/// interactive vim action (`gd`/`gr`/`K`/`a`/`Y`/`[c ]c`/`[f ]f`/`Ctrl-w
/// v`/`Ctrl-w q`) is bound PER PANE (`vimCallbacksForPane`), parameterized
/// by which pane it came from — since only the DOM-focused pane's own CM6
/// keymap instance can ever dispatch a keypress, "which pane fired this"
/// and "the focused pane" are always the same value by construction.
///
/// **Shared inspector state follows focus**: blame/annotations/history/peek
/// (the right rail's content) are each a SINGLE hook instance, keyed to
/// `focusedPath`/`focusedRef` (whichever pane currently has focus) rather
/// than duplicated per pane — switching focus re-points the whole rail at
/// the newly-focused pane's file. Each CodeView only receives non-null
/// `blameDots`/`ageLines`/`annotationMarkers` while ITS pane is the focused
/// one (`focusedPane === 1|2` gates); the unfocused pane still renders its
/// own highlights + its own file's content, but no gutter markers, and its
/// own cursor→URL sync is simply never fed a selection while unfocused (see
/// `cursorSyncRef1`/`cursorSyncRef2` below) — so only the active view's
/// position is ever bookmarked in the address bar.
///
/// **Position flows through the URL (A3, extended by Wave E)**: pane1's
/// `?line=` is unchanged; pane2's own position lives entirely inside the
/// `?pane2=<path@ref:line>` grammar (`lib/codeUrl.ts`, A2-reserved, Wave E's
/// first consumer) — `parsePane2`/`formatPane2` are the ONLY parser/
/// builder for it. `pane2Loc` is derived PURELY from the URL (a `useMemo`
/// over the raw query param) — there is no separate "is a split open" piece
/// of React state to drift out of sync with it.
export default function ReaderLegacy() {
  const { repo = "" } = useParams<{ repo: string }>();
  const splat = useParams()["*"] ?? "";
  const [searchParams] = useSearchParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();

  const diffMode = splat === DIFF_SENTINEL || splat.endsWith(`/${DIFF_SENTINEL}`);
  const storyMode = !diffMode && (splat === STORY_SENTINEL || splat.endsWith(`/${STORY_SENTINEL}`));
  const historySentinelMode = !diffMode && !storyMode && isHistorySentinel(splat);
  const path = diffMode
    ? splat.slice(0, -DIFF_SENTINEL.length).replace(/\/$/, "")
    : storyMode
      ? splat.slice(0, -STORY_SENTINEL.length).replace(/\/$/, "")
      : historySentinelMode
        ? ""
        : splat;
  const gitRef = searchParams.get("ref") ?? undefined;
  const fromRef = searchParams.get("from") ?? undefined;
  const toRef = searchParams.get("to") ?? undefined;
  const atRef = searchParams.get("at") ?? undefined;
  const lineParam = searchParams.get("line");
  const pane2Param = searchParams.get("pane2");
  // T1 (design-ui.md §5/§9.2) — `?sym=` deep link, resolved on load below.
  const symParam = searchParams.get("sym");

  // F5 — mobile shell (≤860px). `isMobile` is read FIRST so the lazy state
  // initializers below can close over its already-current value (both
  // `useState` calls run within the same initial render pass) — no flash of
  // an open tree/sheet before a resize-driven correction lands.
  const isMobile = useIsMobile();
  // Desktop: the tree aside is open by default (unchanged from pre-F5
  // behavior). Mobile: closed by default — it's promoted to an overlay
  // `MobileDrawer` below, and a drawer that's open on cold load would block
  // the very file the operator followed a permalink to read.
  const [treeVisible, setTreeVisible] = useState(() => !isMobile);
  // F5 — the mobile-only "reader tools" bottom sheet (`.kbc-reader__outline`
  // promoted via CSS, mirrors kb's own `.kb-pinsp` sheet promotion, root
  // CLAUDE.md invariant #30). Always false on desktop in practice (nothing
  // ever sets it there); gated on `isMobile` at every render site rather than
  // reset on breakpoint change, since a `!isMobile` guard already keeps the
  // CSS + `asSheet` prop inert regardless of this flag's value.
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [gotoSel1, setGotoSel1] = useState<GotoSel | null>(null);
  const [gotoSel2, setGotoSel2] = useState<GotoSel | null>(null);
  const [helpOpen, setHelpOpen] = useState(false);
  /// V3.N1 — Recent Locations popup (`g.`).
  const [recentLocsOpen, setRecentLocsOpen] = useState(false);
  // V3.N2 — structure / bookmarks popups + sticky pref + cursor line for crumbs.
  const [structureOpen, setStructureOpen] = useState(false);
  const [mnemonicOpen, setMnemonicOpen] = useState(false);
  /// V3.R2 / R12 — `gh` history-for-selection popup target.
  const [lineHistory, setLineHistory] = useState<{
    path: string;
    line: number;
    lineEnd?: number;
  } | null>(null);
  const [stickyEnabled, setStickyEnabled] = useState(() => loadStickyContext());
  const [paramHintsEnabled, setParamHintsEnabled] = useState(() => loadParamHints());
  const [codeLensesEnabled, setCodeLensesEnabled] = useState(() => loadCodeLenses());
  // SH.C3 — reading-mode prefs (line wrap + CM6 font size), same
  // useState-lazy-init + prefs.ts round-trip pattern as the three above.
  const [wrapEnabled, setWrapEnabled] = useState(() => loadWrap());
  const [readerFontSize, setReaderFontSize] = useState(() => loadReaderFontSize());
  const [cursorLineUi, setCursorLineUi] = useState(1);
  const [copiedTick, setCopiedTick] = useState(0);
  /// Suppress the next file-open `recordJump` when the navigation was
  /// driven by Ctrl-o / Ctrl-i (the jump list already moved its pointer;
  /// re-recording would clobber the forward half).
  const skipNavRecordRef = useRef(false);
  const [vimStat, setVimStat] = useState<{ mode: string; pending: string }>({ mode: "normal", pending: "" });
  const nonceRef = useRef(0);
  const treeRef = useRef<FileTreeHandle | null>(null);
  const inspectorRef = useRef<InspectorRailHandle | null>(null);
  const codeViewRef1 = useRef<CodeViewHandle | null>(null);
  const codeViewRef2 = useRef<CodeViewHandle | null>(null);
  // Phase E4 — the "+ Set" capture affordance's own idea of "the current
  // selection": each pane's latest `onSelectionLines` report, kept purely so
  // a click on "+ Set" can grab it at that moment (`AddToSetMenu`'s
  // `getSelection` prop) — mirrors `cursorLineRef1`/`cursorLineRef2` below
  // (a ref, not state, since nothing here needs to RE-RENDER on every
  // selection change).
  const lastSelRef1 = useRef<{ start: number; end: number } | null>(null);
  const lastSelRef2 = useRef<{ start: number; end: number } | null>(null);

  const isFile = path !== "" && !path.endsWith("/");
  const activeFile = isFile && !diffMode && !storyMode ? path : undefined;
  const file = useFile(repo, activeFile, gitRef);
  const lenses1 = useLenses(
    repo,
    activeFile,
    file.data?.blob_hash,
    codeLensesEnabled && !!activeFile,
  );
  const repos = useRepos();
  const repoRoot = repos.data?.repos.find((r) => r.name === repo)?.path;

  // Phase G2 — the repo-state banner + the CM6 conflict-marker line tint.
  // `RepoStateBanner` fetches its OWN copy of `GET /api/repo-state` (the
  // SAME `["repo-state", repo]` query key — React Query dedupes to one
  // network call), so this component only needs the `conflicted` set to
  // decide whether EITHER open pane's file gets the tint.
  const repoState = useRepoState(repo);
  const conflictedPaths = useMemo(() => new Set(repoState.data?.conflicted ?? []), [repoState.data]);

  // --- Wave E — pane2 is derived PURELY from the URL, gated the same way
  // `activeFile` is (a file, not a diff) — a split has no meaning over the
  // diff view, story mode, or a directory listing.
  const canSplit = isFile && !diffMode && !storyMode;
  const pane2Loc = useMemo<PaneLoc | null>(() => (canSplit ? parsePane2(pane2Param) : null), [canSplit, pane2Param]);
  const pane2File = useFile(repo, pane2Loc?.path, pane2Loc?.ref);
  const lenses2 = useLenses(
    repo,
    pane2Loc?.path,
    pane2File.data?.blob_hash,
    codeLensesEnabled && !!pane2Loc?.path,
  );

  const [focusedPane, setFocusedPane] = useState<1 | 2>(1);
  // A split that closes (or was never opened) while pane2 was focused has
  // nowhere for that focus to point — fall back to pane1.
  useEffect(() => {
    if (!pane2Loc && focusedPane === 2) setFocusedPane(1);
  }, [pane2Loc, focusedPane]);
  // A fresh pane's vim state always starts at "normal" — without this, a
  // status chip left over from the PREVIOUS pane's visual-mode selection
  // would misleadingly linger after focus moves.
  useEffect(() => {
    setVimStat({ mode: "normal", pending: "" });
  }, [focusedPane]);

  const focusedPath = focusedPane === 1 ? activeFile : pane2Loc?.path;
  const focusedRef = focusedPane === 1 ? gitRef : pane2Loc?.ref;

  const workingSet = useWorkingSet(repo);
  // "touch on every file open," regardless of HOW it was opened (tree
  // click, Shift+Enter, a hard-navigated permalink, `gd`, a breadcrumb…) —
  // keying this off the resolved path itself (not each individual call
  // site) is what makes that guarantee total rather than best-effort.
  useEffect(() => {
    if (activeFile) workingSet.touch(activeFile);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeFile]);
  useEffect(() => {
    if (pane2Loc?.path) workingSet.touch(pane2Loc.path);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pane2Loc?.path]);

  // V3.N1 — record a jump on every file open (path change). Skipped when
  // the navigation came from Ctrl-o / Ctrl-i (`skipNavRecordRef`).
  useEffect(() => {
    if (!activeFile || !repo) return;
    if (skipNavRecordRef.current) {
      skipNavRecordRef.current = false;
      return;
    }
    const line = parseLineParam(lineParam)?.start ?? 1;
    recordJump({ repo, path: activeFile, line, snippet: "" });
    // Only re-fire on path/repo change — not every line-param tick.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, activeFile]);

  /// The (repo-relative) path/ref/CodeView-ref this pane is currently
  /// showing — the one place every per-pane handler below resolves "which
  /// pane am I acting on" from, so pane1/pane2 stay symmetric rather than
  /// one being the "real" implementation and the other a bolt-on copy.
  function paneRepoPath(pane: 1 | 2) {
    return pane === 1
      ? { path: activeFile, ref: gitRef, viewRef: codeViewRef1 }
      : { path: pane2Loc?.path, ref: pane2Loc?.ref, viewRef: codeViewRef2 };
  }

  /// Phase E4 — the FOCUSED pane's last-reported selection lines, or `null`
  /// (no selection — a plain collapsed cursor). `AddToSetMenu`'s
  /// `getSelection` prop; read once at click time, never re-rendered on.
  function currentPaneSelection(): { start: number; end: number } | null {
    return focusedPane === 1 ? lastSelRef1.current : lastSelRef2.current;
  }

  /// Build a reader URL, defaulting EITHER slot to its current value unless
  /// explicitly overridden — every Wave E navigation (splits open/close,
  /// per-pane history stepping, tree/working-set opens, peek/linkify
  /// jumps) goes through this ONE function, so "leave whatever pane I
  /// didn't touch exactly as it is" is enforced here rather than re-derived
  /// at each call site. `pane2: null` clears it; `pane2: undefined` (or
  /// omitted) keeps whatever's currently open.
  function buildReaderUrl(next: {
    pane1?: { path: string; ref?: string; line?: PaneLineSel };
    pane2?: PaneLoc | null;
  }): string {
    const currentPane1Line = parseLineParam(lineParam) ?? undefined;
    const pane1 = next.pane1 ?? { path: activeFile ?? "", ref: gitRef, line: currentPane1Line };
    const pane2 = next.pane2 === undefined ? pane2Loc ?? undefined : (next.pane2 ?? undefined);
    return codeUrl({ repo, path: pane1.path, ref: pane1.ref, line: pane1.line, pane2 });
  }

  /// A resolved candidate/peek-row landing: same-repo lands in `pane`
  /// (preserving whatever the OTHER pane is showing); a different repo
  /// always opens as a fresh single-pane view there — a split is scoped to
  /// one repo (`PaneLoc` carries no `repo` field, see `lib/codeUrl.ts`), so
  /// crossing repos closes it regardless of which pane asked.
  function navigateToCandidate(pane: 1 | 2, candRepo: string, candPath: string, line: number) {
    // V3.N1 — same-file landings need an explicit record (the file-open
    // effect only fires on path change). Cross-file opens are recorded by
    // that effect once `activeFile` updates.
    const panePath = pane === 1 ? activeFile : pane2Loc?.path;
    if (candRepo === repo && candPath === panePath) {
      recordJump({ repo: candRepo, path: candPath, line, snippet: "" });
    }
    if (candRepo !== repo) {
      navigate(codeUrl({ repo: candRepo, path: candPath, line }));
      return;
    }
    if (pane === 1) navigate(buildReaderUrl({ pane1: { path: candPath, line } }));
    else navigate(buildReaderUrl({ pane2: { path: candPath, line } }));
  }

  /// Open `targetPath` (same repo) into the focused pane, or explicitly
  /// into pane2 (`Shift+Enter`/Shift+click/middle-click) — the shared entry
  /// point for the file tree and the working-set strip. Inherits pane1's
  /// current `?ref=` either way (mirrors the pre-Wave-E tree click, which
  /// already preserved `gitRef` across a plain file-to-file navigation).
  /// Jump recording is owned by the file-open effect on `activeFile`.
  function openPath(targetPath: string, target: "focused" | "pane2") {
    const wantPane2 = target === "pane2" || focusedPane === 2;
    if (wantPane2) {
      navigate(buildReaderUrl({ pane2: { path: targetPath, ref: gitRef } }));
      setFocusedPane(2);
    } else {
      navigate(buildReaderUrl({ pane1: { path: targetPath, ref: gitRef } }));
      setFocusedPane(1);
    }
  }

  /// Snapshot the focused pane's current location for jump-list / recent.
  function currentJumpLocation(): { repo: string; path: string; line: number; snippet: string } {
    const path = focusedPane === 1 ? activeFile : pane2Loc?.path;
    const line = focusedPane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    return { repo, path: path ?? "", line: line || 1, snippet: "" };
  }

  function navigateToNavLocation(target: { repo: string; path: string; line: number }) {
    skipNavRecordRef.current = true;
    if (target.repo !== repo) {
      navigate(codeUrl({ repo: target.repo, path: target.path, line: target.line }));
      return;
    }
    if (focusedPane === 2 && pane2Loc) {
      navigate(buildReaderUrl({ pane2: { path: target.path, ref: pane2Loc.ref, line: target.line } }));
    } else {
      navigate(buildReaderUrl({ pane1: { path: target.path, ref: gitRef, line: target.line } }));
    }
  }

  function handleJumpBack() {
    const cur = currentJumpLocation();
    if (!cur.path) return;
    const target = goBack(cur);
    if (target) navigateToNavLocation(target);
  }

  function handleJumpForward() {
    const target = goForward();
    if (target) navigateToNavLocation(target);
  }

  // --- T1 — `?sym=` deep link resolution (design-ui.md §5/§9.2) -----------
  // `GET /api/resolve-symbol` is scoped to ONE repo (no cross-repo fan-out —
  // `symbol_addr.rs`'s own doc), so a `found` result always lands in PANE1
  // of THIS repo, never a cross-repo `navigate` the way `gd`'s candidates
  // sometimes do. `found: true` re-centers on the resolved path/line
  // (dropping any `?ref=` — resolve-symbol, like `/api/resolve`, always
  // answers against the CURRENT working tree, never a historical ref);
  // `found: false` (or a network failure) toasts the reason and just STRIPS
  // `sym=` from the URL, leaving pane1 exactly where it already is — the
  // URL's own `path`/`line` was always the honest fallback anchor, never a
  // dead end. `symResolvedRef` guards against React 18 dev-mode's
  // double-invoke re-firing the same resolve twice; by construction the
  // effect never loops (both branches remove `sym=` from the next URL).
  const symResolvedRef = useRef<string | null>(null);
  useEffect(() => {
    if (!symParam || !repo) return;
    if (symResolvedRef.current === symParam) return;
    symResolvedRef.current = symParam;
    let cancelled = false;
    function stripSym() {
      const next = new URLSearchParams(window.location.search);
      next.delete("sym");
      const qs = next.toString();
      navigate({ search: qs ? `?${qs}` : "" }, { replace: true });
    }
    (async () => {
      try {
        const result = await fetchResolveSymbol(repo, symParam);
        if (cancelled) return;
        if (result.found && result.path) {
          navigate(
            buildReaderUrl({ pane1: { path: result.path, ref: undefined, line: result.line } }),
            { replace: true },
          );
        } else {
          toast.warn(`sym=${symParam} didn't resolve${result.reason ? `: ${result.reason}` : ""} — staying put`);
          stripSym();
        }
      } catch (e) {
        if (cancelled) return;
        toast.err(`sym= resolve failed: ${e instanceof Error ? e.message : String(e)}`);
        stripSym();
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [symParam, repo]);

  // --- A3 — URL ?line= → pane1's buffer selection -------------------------
  // The `line` param is the source of truth for externally-driven position
  // (search-lane landings, permalinks, in-app links). Writes that ORIGINATED
  // from the cursor itself (cursorUrlSync below) are skipped via
  // `lastSyncedLineRef`, or the URL echo would yank the cursor to the
  // range's start half a second after every move.
  const lastSyncedLineRef = useRef<string | null>(null);
  useEffect(() => {
    if (lineParam === null || lineParam === lastSyncedLineRef.current) return;
    const range = parseLineParam(lineParam);
    if (!range) return;
    nonceRef.current += 1;
    setGotoSel1({ start: range.start, end: range.end, nonce: nonceRef.current });
  }, [lineParam, repo, path]);

  // --- Wave E — pane2=…:line → pane2's buffer selection -------------------
  // Same "external URL drives the selection" idiom as pane1's own effect
  // above, scoped to the `:line` suffix inside `pane2=` — fires whenever
  // pane2's own path/ref/line change (a fresh split, `[f ]f`, a history
  // step, or a hard-navigated `?pane2=` URL), skipped when it's an echo of
  // pane2's OWN cursor sync (`lastSyncedPane2LineRef`, set inside
  // `createPane2CursorUrlSync`'s `replace` callback below).
  const lastSyncedPane2LineRef = useRef<string | null>(null);
  useEffect(() => {
    if (!pane2Loc) return;
    const lineStr = pane2Loc.line !== undefined ? formatLineParam(pane2Loc.line) : "";
    if (lineStr === "" || lineStr === lastSyncedPane2LineRef.current) return;
    const range = parseLineParam(lineStr);
    if (!range) return;
    nonceRef.current += 1;
    setGotoSel2({ start: range.start, end: range.end, nonce: nonceRef.current });
  }, [pane2Loc]);

  // --- A3 — pane1 buffer selection → URL ?line= (debounced) ---------------
  // The sync object lives for as long as pane1 has a file open, regardless
  // of focus — but its `onSelection` is only ever CALLED while pane1 is the
  // focused pane (see the `onSelectionLines` prop below): the unfocused
  // pane's cursor simply never feeds a pending write, so only the active
  // view's position is ever bookmarked in the address bar.
  const cursorSyncRef1 = useRef<CursorUrlSync | null>(null);
  useEffect(() => {
    if (!activeFile) return;
    const sync = createCursorUrlSync({
      replace: (search) => {
        lastSyncedLineRef.current = new URLSearchParams(search).get("line");
        navigate({ search }, { replace: true });
      },
      getSearch: () => window.location.search,
    });
    cursorSyncRef1.current = sync;
    return () => {
      // A pending debounce must not fire after navigating to another
      // file/route — it would stamp the OLD file's line onto the new URL.
      sync.dispose();
      cursorSyncRef1.current = null;
    };
    // `navigate` is identity-unstable across renders by design; the sync
    // only needs A working navigate, not the latest closure.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, activeFile]);

  // --- Wave E — pane2 buffer selection → URL pane2=…:line (debounced) -----
  // Sibling of the sync above, scoped to pane2's own `:line` suffix — same
  // "lives whenever pane2 has a file open, fed only while focused" split.
  const cursorSyncRef2 = useRef<CursorUrlSync | null>(null);
  useEffect(() => {
    if (!pane2Loc) return;
    const sync = createPane2CursorUrlSync({
      getPaneBase: () => (pane2Loc ? { path: pane2Loc.path, ref: pane2Loc.ref } : null),
      replace: (search) => {
        const parsed = parsePane2(new URLSearchParams(search).get("pane2"));
        lastSyncedPane2LineRef.current = parsed?.line !== undefined ? formatLineParam(parsed.line) : "";
        navigate({ search }, { replace: true });
      },
      getSearch: () => window.location.search,
    });
    cursorSyncRef2.current = sync;
    return () => {
      sync.dispose();
      cursorSyncRef2.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [repo, pane2Loc?.path, pane2Loc?.ref]);

  // --- A3 — focus follows the file: opening a file lands focus in the
  // buffer (pending flag set on path/ref change, consumed once the blob is
  // actually rendered) — but never steals focus from an input the user is
  // typing in (e.g. the tree's quick-filter during a live-mirror refetch).
  // Wave E: mirrored per pane, so opening/switching EITHER pane's file
  // auto-focuses THAT pane (the "split with self"/Shift+Enter/working-set
  // gestures all rely on this to land keyboard focus where the operator
  // just asked to look).
  const pendingFocusRef1 = useRef(false);
  useEffect(() => {
    pendingFocusRef1.current = activeFile !== undefined;
  }, [repo, activeFile, gitRef]);
  const blobHash1 = file.data?.encoding === "utf8" ? file.data.blob_hash : undefined;
  useEffect(() => {
    if (!pendingFocusRef1.current || !blobHash1) return;
    if (isEditableTarget(document.activeElement)) return;
    pendingFocusRef1.current = false;
    codeViewRef1.current?.focus();
  }, [blobHash1, gitRef]);

  const pendingFocusRef2 = useRef(false);
  useEffect(() => {
    pendingFocusRef2.current = pane2Loc?.path !== undefined;
  }, [repo, pane2Loc?.path, pane2Loc?.ref]);
  const blobHash2 = pane2File.data?.encoding === "utf8" ? pane2File.data.blob_hash : undefined;
  useEffect(() => {
    if (!pendingFocusRef2.current || !blobHash2) return;
    if (isEditableTarget(document.activeElement)) return;
    pendingFocusRef2.current = false;
    codeViewRef2.current?.focus();
  }, [blobHash2, pane2Loc?.ref]);

  // --- W4.4/Wave C — the Provenance overlay: off | dots | age -------------
  // `dots` is the original W4.4 disclosure-ladder gutter; `age` is Wave C's
  // line-background heatmap (`lib/ageHeatmap.ts`) — both ride the SAME
  // `GET /api/blame` fetch (`blame` below), but only `dots` additionally
  // needs the per-sha join-ladder attribution fan-out (`attributions`):
  // age tinting reads `BlameRegion.author_time` directly, no join lookup.
  // Wave E: keyed on `focusedPath`/`focusedRef` (the FOCUSED pane), not
  // pane1 unconditionally — the shared inspector rail follows focus.
  const [provenanceMode, setProvenanceMode] = useState<ProvenanceMode>("off");
  const provenanceOn = provenanceMode !== "off";
  const blame = useBlame(repo, focusedPath, focusedRef, provenanceOn);
  const attributions = useBlameAttributions(repo, focusedPath, blame.data?.regions, provenanceMode === "dots");
  const attributionBySha = useMemo(() => {
    const map = new Map<string, AttributionOut>();
    for (const [sha, why] of attributions.bySha) map.set(sha, why.attribution);
    return map;
  }, [attributions.bySha]);
  const blameDots = useMemo<Map<number, BlameDotInfo> | null>(() => {
    if (provenanceMode !== "dots" || !blame.data) return null;
    return buildLineDots(blame.data.regions, attributionBySha);
  }, [provenanceMode, blame.data, attributionBySha]);
  const ageLines = useMemo<Map<number, AgeLineInfo> | null>(() => {
    if (provenanceMode !== "age" || !blame.data) return null;
    return buildAgeLineBuckets(blame.data.regions);
  }, [provenanceMode, blame.data]);

  // PRR-U9 — diagnostics gutter marks, keyed on the FOCUSED pane's path
  // (same "follows focus" convention `blameDots`/`ageLines` use above).
  // `useDiagnostics` itself gates on the repo's intel provider covering
  // `focusedPath`'s language, so this fetch is a no-op for most repos/files.
  const diagnostics = useDiagnostics(repo, focusedPath);
  const diagMarks = useMemo<Map<number, DiagnosticGutterMark> | null>(() => {
    if (!diagnostics.data?.diagnostics) return null;
    return diagnosticGutterMarks(diagnostics.data.diagnostics);
  }, [diagnostics.data]);

  const [ladder, dispatchLadder] = useReducer(ladderReducer, initialLadderState);
  const [hoverChip, setHoverChip] = useState<
    { line: number; rect: DOMRect; label: string; solid: boolean } | null
  >(null);

  function handleBlameHover(line: number, rect: DOMRect) {
    dispatchLadder({ type: "hover", line });
    const dot = blameDots?.get(line);
    setHoverChip(dot ? { line, rect, label: dot.label, solid: dot.solid } : null);
  }
  function handleBlameUnhover(line: number) {
    dispatchLadder({ type: "unhover", line });
    setHoverChip((c) => (c && c.line === line ? null : c));
  }
  function handleBlameClick(line: number) {
    dispatchLadder({ type: "click", line });
    setHoverChip(null);
    inspectorRef.current?.openTab("provenance");
  }

  const openRegion =
    ladder.openLine !== null && blame.data ? regionCoveringLine(blame.data.regions, ladder.openLine) : undefined;
  const openWhy = openRegion ? attributions.bySha.get(openRegion.sha) : undefined;
  const whyPanel =
    ladder.openLine !== null ? (
      <>
        <WhyPanel
          repo={repo}
          repoRoot={repoRoot}
          path={focusedPath ?? ""}
          line={ladder.openLine}
          region={openRegion}
          why={openWhy}
        />
        {/* CT-E2 — the file's session timeline (incl. attention-gap divider
            beats) below the per-line why. Rendered inside the `whyPanel`
            node so it only MOUNTS (and only fetches — `hooks/useStory.ts`)
            while the provenance tab is actually open. */}
        {focusedPath !== undefined && <StoryTimeline repo={repo} path={focusedPath} />}
      </>
    ) : null;

  // Reset the ladder's open panel + any lingering hover chip on a file/repo
  // switch (INCLUDING a focus switch between panes — a stale line number
  // from the PREVIOUSLY focused pane's blame result has nothing to show for
  // the newly-focused one).
  useEffect(() => {
    dispatchLadder({ type: "closePanel" });
    setHoverChip(null);
  }, [repo, focusedPath, focusedRef]);

  // --- W4.6 — annotations gutter + panel -----------------------------------
  // Wave E: keyed on the FOCUSED pane's path — the annotations panel/gutter
  // markers follow focus, same as Provenance above.
  const annotations = useAnnotations(repo, focusedPath);
  // F5 — hoisted out of the InspectorRail prop expression so the mobile
  // entry button's badge (Reader's own header) and the rail's own tab badge
  // read the exact same count without computing it twice.
  const unresolvedAnnotationsCount = annotations.data
    ? unresolvedCount(annotations.data.annotations)
    : 0;
  const annotationMarkers = useMemo<Map<number, LineMarkerSpec> | null>(() => {
    if (!annotations.data) return null;
    const byLine = annotationsByLine(annotations.data.annotations);
    const out = new Map<number, LineMarkerSpec>();
    for (const [line, list] of byLine) {
      out.set(line, {
        className: "kbc-annot-dot",
        title: annotationGutterTitle(list),
      });
    }
    return out;
  }, [annotations.data]);
  const [annotationActiveLine, setAnnotationActiveLine] = useState<number | null>(null);
  // Phase D — the other end of a visual-mode `a` range selection; reset to
  // `null` by every SINGLE-line entry point (gutter click, the outside-
  // buffer `a` keybinding) so a stale range never leaks into the next
  // composer invocation.
  const [annotationActiveLineEnd, setAnnotationActiveLineEnd] = useState<number | null>(null);
  const cursorLineRef1 = useRef(1);
  const cursorLineRef2 = useRef(1);

  // V3.N2 — bookmarks for the whole repo (badge + gm toggle).
  const bookmarksQ = useBookmarks(repo);
  const createBookmarkMut = useCreateBookmark(repo);
  const deleteBookmarkMut = useDeleteBookmark(repo);
  const bookmarkCount = bookmarksQ.data?.bookmarks.length ?? 0;

  function handleAnnotationClick(line: number) {
    setAnnotationActiveLine(line);
    setAnnotationActiveLineEnd(null);
    inspectorRef.current?.openTab("annotations");
  }

  // --- Wave C — History inspector tab + `[c ]c` time travel ----------------
  // Fetched whenever the FOCUSED pane has a file open (NOT gated on the
  // History tab being the active one) — the `[c`/`]c` vim binding must work
  // immediately, before the tab is ever opened, same as the annotations
  // gutter's own always-fetch discipline above.
  const fileHistory = useFileHistory(repo, focusedPath, focusedPath !== undefined);
  const historyEntries = fileHistory.data?.entries ?? [];
  const historyCurIndex = currentHistoryIndex(historyEntries, focusedRef);

  /// Step (or jump directly to) `sha` in `pane`'s OWN ref slot — pane1's
  /// route `?ref=`, or pane2's `pane2=…ref…` — carrying that pane's live
  /// cursor line forward, leaving the OTHER pane untouched.
  function navigateHistory(pane: 1 | 2, sha: string | undefined) {
    if (pane === 1) {
      if (!activeFile) return;
      navigate(buildReaderUrl({ pane1: { path: activeFile, ref: sha, line: cursorLineRef1.current } }));
    } else {
      if (!pane2Loc) return;
      navigate(buildReaderUrl({ pane2: { path: pane2Loc.path, ref: sha, line: cursorLineRef2.current } }));
    }
  }

  function handleHistoryStep(pane: 1 | 2, dir: -1 | 1) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    const result = historyStepTarget(historyEntries, historyCurIndex, dir);
    if (result.kind === "warn") {
      toast.warn(result.message);
      return;
    }
    navigateHistory(pane, result.sha);
  }

  const historyPanel = focusedPath ? (
    <HistoryPanel
      repo={repo}
      entries={historyEntries}
      currentRef={focusedRef}
      onNavigate={(sha) => navigateHistory(focusedPane, sha)}
      onOpenStory={(sha) => navigate(storyUrl(repo, focusedPath, sha))}
      isLoading={fileHistory.isLoading}
      truncated={fileHistory.data?.truncated ?? false}
    />
  ) : null;

  // DCB W3.B — the always-visible "Cited by" slot (InspectorRailProps'
  // `citedBy`, NOT a 7th tab). Gated purely on a file being open; `CitedBy`
  // owns its own `useDocRefs` fetch and renders nothing itself when that
  // fetch resolves to zero claims, so this never needs a second pre-fetch
  // here just to decide null-vs-render (the annotations badge above
  // pre-fetches because it feeds a COUNT into the mobile entry button;
  // nothing else here needs `doc_refs` data outside `CitedBy` itself).
  // A `focusedPath`-derived `key` (m6, W3.B.R review): `CitedBy`'s
  // `useState(false)` disclosure is per-MOUNT, not per-path — without a key
  // that switches on file/pane change, expanding the list on one file
  // leaves it expanded when the reader focuses a different file,
  // contradicting the collapsed-by-default spec. Keying on path forces a
  // remount (fresh `open: false`) on every file switch. PRR-U8R fix: the
  // key carries a PER-SLOT prefix (`citedby:`/`framework:`/`diagnostics:`
  // below) — `citedBy`/`frameworkCard`/`diagnosticsCard` are three DIFFERENT
  // sibling children inside InspectorRail's returned array (`{citedBy}
  // {frameworkCard}{diagnosticsCard}`), so a bare `key={focusedPath}`
  // shared verbatim across all three collided: React's array reconciler
  // (`reconcileChildrenArray`) builds its "existing children" lookup keyed
  // by `key`, and inserting three DIFFERENT-typed fibers under the SAME key
  // silently overwrites earlier entries in that map — the discarded entry
  // never gets scheduled for deletion, leaving a stale fiber (and its DOM
  // node) permanently behind alongside the new one. Reproduced with a
  // production build (StrictMode on OR off — not a dev-only artifact):
  // once both `citedBy` and `frameworkCard` render real content for the
  // same file, `.kbc-inspector` ends up with TWO `[data-kbc-citedby]`
  // divs. Unique per-slot keys make each slot's own remount independent —
  // no more cross-slot collision.
  const citedBy = focusedPath ? (
    <CitedBy key={`citedby:${focusedPath}`} repo={repo} path={focusedPath} />
  ) : null;

  // T1 (design-ui.md §9.4a) — the Framework card, mounted the SAME
  // always-visible way `citedBy` is above: `FrameworkCard` owns its own
  // `useFrameworkEdges` fetch and renders nothing at all until that fetch
  // lands (see that component's doc), so — like `citedBy` — this never
  // needs a second pre-fetch here just to decide null-vs-render.
  const frameworkCard = focusedPath ? (
    <FrameworkCard key={`framework:${focusedPath}`} repo={repo} path={focusedPath} />
  ) : null;

  // PRR-U9 (design-addendum-2.md §D) — the Diagnostics card, mounted the
  // SAME always-visible way `frameworkCard`/`citedBy` are above.
  // `DiagnosticsCard` owns its own `useDiagnostics` fetch and renders
  // nothing at all when no provider covers this file's language (see that
  // component's doc). The jump handler mirrors `onGotoAnnotationLine`'s own
  // "jump then re-focus the buffer" idiom (`jumpToLine`/`paneRepoPath`
  // below) — a diagnostic row is always in the CURRENTLY open file, never a
  // cross-file navigation.
  const diagnosticsCard = focusedPath ? (
    <DiagnosticsCard
      key={`diagnostics:${focusedPath}`}
      repo={repo}
      path={focusedPath}
      onJumpLine={(line, lineEnd) => {
        jumpToLine(focusedPane, line, lineEnd);
        paneRepoPath(focusedPane).viewRef.current?.focus();
      }}
    />
  ) : null;

  // --- W4.5 — live-mirror auto-refresh heuristic, one instance per pane ----
  // Wave E: `registerOpenFileForLiveMirror`'s registry is now keyed
  // (`api/queryClient.ts`), so both panes' open files are independently
  // protected from a silent refetch while their viewer is dirty.
  const viewerDirtyRef1 = useRef(false);
  const liveMirror1 = useLiveMirror("pane1", repo, activeFile, viewerDirtyRef1, () => void file.refetch());
  const viewerDirtyRef2 = useRef(false);
  const liveMirror2 = useLiveMirror("pane2", repo, pane2Loc?.path, viewerDirtyRef2, () => void pane2File.refetch());

  function refreshTree() {
    void queryClient.invalidateQueries({ predicate: (q) => q.queryKey[1] === repo });
    liveMirror1.dismissHeadMoved();
    liveMirror2.dismissHeadMoved();
  }

  // --- A3 — vim action callbacks (dispatched from inside a buffer) --------
  const copiedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  function flashCopied() {
    setCopiedTick((t) => t + 1);
    if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
    copiedTimerRef.current = setTimeout(() => setCopiedTick(0), 1600);
  }
  useEffect(() => () => {
    if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
  }, []);

  /// `Y` — Wave E gives the two panes deliberately DIFFERENT semantics
  /// (documented per the milestone brief): pane1-focused copies the reader's
  /// FULL current view (pane1 + whatever pane2 is open, if any) as one
  /// shareable permalink — pane1 is "the primary file," so sharing it
  /// naturally carries its split along. Pane2-focused instead PROMOTES
  /// pane2's own file/ref into a plain pane1 URL with no `pane2=` at all —
  /// pane2-focused means "I'm reading THIS file right now," and a recipient
  /// with no split context of their own should land exactly on it, not on
  /// pane1's (possibly unrelated) file with pane2 reduced to an easy-to-miss
  /// footnote. Either way only the LINE is refreshed from the live cursor
  /// (`sel`) rather than re-reading the URL, which may be up to the 500ms
  /// debounce behind — same immediacy the pre-Wave-E single-pane `Y` relied on.
  function handlePermalinkForPane(pane: 1 | 2, sel: LineSel) {
    const line = sel.lineEnd !== undefined ? { start: sel.line, end: sel.lineEnd } : sel.line;
    if (pane === 1) {
      if (!activeFile) return;
      copyToClipboard(
        permalinkFor(window.location.origin, { repo, path: activeFile, ref: gitRef, line, pane2: pane2Loc ?? undefined }),
      );
    } else {
      if (!pane2Loc) return;
      copyToClipboard(permalinkFor(window.location.origin, { repo, path: pane2Loc.path, ref: pane2Loc.ref, line }));
    }
    flashCopied();
  }

  // --- B1/B3 — gd / gr / K: the resolve+peek panel --------------------------
  // B1 shipped tier-0 clickable code over `/api/defs` + `/api/xrefs`. B3
  // adds `/api/resolve` (position-based, occurrence-aware) as `gd`'s and
  // `K`'s PRIMARY path — `gd` falls back to the original name-based
  // `fetchDefs` only when resolve itself fails (404/older daemon/network);
  // `gr` stays on `/api/xrefs` unchanged (resolve has no refs listing).
  // None of these carry a `ref` parameter (they resolve against the
  // currently-indexed working tree, not whatever historical `ref` this
  // route happens to be viewing), so every navigation built from a peek
  // result deliberately OMITS the source pane's current `?ref=`.
  //
  // Wave E: the peek PANEL itself is a single shared overlay (not one per
  // pane) — `peekOwnerPaneRef` remembers which pane's own action opened it,
  // so activating a row (or closing the panel) routes back to THAT pane.
  const [peek, dispatchPeek] = useReducer(peekReducer, initialPeekState);
  const [peekAnchor, setPeekAnchor] = useState<PeekAnchor | null>(null);
  const peekOwnerPaneRef = useRef<1 | 2>(1);
  // Bumped on every gd/gr/K press; a resolved fetch whose id has since been
  // superseded by a newer press is dropped — same "ignore a stale async
  // result" discipline as `lastSyncedLineRef`/`liveMirror` elsewhere in this
  // file, since nothing else here cancels the in-flight `fetch` itself.
  const peekReqRef = useRef(0);
  // T1 — the position `K`'s hover card was opened for, so the card's own
  // "usages"/"callers" footer buttons (design-ui.md §9.1) can re-fire
  // `handleFindRefs`/`handleHierarchyCallers` against it. `null` until the
  // first `K` press; never read while `peek.card` is unset.
  const lastHoverPosRef = useRef<{ pane: 1 | 2; pos: WordPos } | null>(null);

  function capturePeekAnchor(pane: 1 | 2): PeekAnchor | null {
    return paneRepoPath(pane).viewRef.current?.cursorCoords() ?? null;
  }

  // B3 — `gd` NOW resolves position-first (`/api/resolve` at the cursor's
  // own WordPos — `col` is the word's first char, exactly what resolve
  // expects a position ON the identifier to be) instead of name-first. A
  // single candidate jumps straight there (same-file via `jumpToLine`, else
  // `navigateToCandidate` — same B1 reasoning above); several open the
  // peek panel with one row per candidate, each carrying its own
  // `precision` badge (`PeekPanel`'s `PrecisionBadge`); ZERO candidates
  // opens the panel empty (resolve answered honestly — "no definition
  // here" — not a reason to fall back). The fallback below fires ONLY when
  // resolve itself fails (404 on an older daemon, network error, etc.) —
  // feature-detected per attempt, nothing cached — so `gd` never goes dead.
  async function handleGotoDef(pane: 1 | 2, pos: WordPos) {
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (!panePath) return;
    peekOwnerPaneRef.current = pane;
    dispatchHier({ type: "CLOSE" });
    const reqId = ++peekReqRef.current;

    try {
      const resolved = await fetchResolve({ repo, path: panePath, line: pos.line, col: pos.col });
      if (reqId !== peekReqRef.current) return;
      if (resolved.candidates.length === 1) {
        const c = resolved.candidates[0];
        if (c.repo === repo && c.path === panePath) {
          jumpToLine(pane, c.line);
          viewRef.current?.focus();
        } else {
          navigateToCandidate(pane, c.repo, c.path, c.line);
        }
        return;
      }
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({
        type: "OPEN_WITH_ROWS",
        mode: "defs",
        word: resolved.ident,
        rows: resolved.candidates.map(resolveCandidateToRow),
        approximate: false,
        note: resolved.note,
      });
      return;
    } catch {
      // resolve unavailable (404/older daemon/network) — fall through to
      // B1's original name-based path below.
    }
    if (reqId !== peekReqRef.current) return;

    try {
      // Cross-repo on purpose (no `repo` filter) — a hit living in a
      // DIFFERENT configured repo is still a valid landing (Deliverable 2's
      // cross-repo requirement); `PeekPanel` shows the repo name whenever a
      // row's repo differs from the one currently open.
      const defs = await fetchDefs(undefined, pos.word);
      if (reqId !== peekReqRef.current) return;
      const exact = singleExactMatch(defs);
      if (exact) {
        navigateToCandidate(pane, exact.repo, exact.path, exact.line_start);
        return;
      }
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({ type: "OPEN_WITH_ROWS", mode: "defs", word: pos.word, rows: defRowsFrom(defs), approximate: !defs.exact });
    } catch (e) {
      if (reqId !== peekReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setPeekAnchor(capturePeekAnchor(pane));
      dispatchPeek({ type: "OPEN", mode: "defs", word: pos.word });
      dispatchPeek({ type: "SET_ERROR", message });
      // F3a — the panel's own error row (above) is the primary surface, but
      // toast too: `gd` can be pressed anywhere in the buffer, and a panel
      // dismissed a moment later (Esc, or a fast subsequent keypress) would
      // otherwise leave no trace that the lookup actually failed.
      toast.err(`gd failed: ${message}`);
    }
  }

  async function handleFindRefs(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    peekOwnerPaneRef.current = pane;
    dispatchHier({ type: "CLOSE" });
    setPeekAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "OPEN", mode: "refs", word: pos.word });
    const reqId = ++peekReqRef.current;
    try {
      const refs = await fetchXrefs(repo, pos.word);
      if (reqId !== peekReqRef.current) return;
      dispatchPeek({ type: "SET_ROWS", rows: refRowsFrom(refs), approximate: true, note: refs.note });
    } catch (e) {
      if (reqId !== peekReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchPeek({ type: "SET_ERROR", message });
      toast.err(`gr failed: ${message}`);
    }
  }

  // B3 — `K` becomes the "what AND why" provenance hover: the TOP
  // `/api/resolve` candidate rendered as a card (`PeekPanel`'s
  // `HoverCardView`), followed by ONE provenance line from `/api/why` at
  // that candidate's def location. Two sequential awaits (resolve, then
  // why) — the `peekReqRef` stale-guard is checked after EACH one, so a
  // newer `gd`/`gr`/`K` press cleanly drops both a slow resolve and a slow
  // why that are no longer relevant.
  async function handleHover(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    peekOwnerPaneRef.current = pane;
    // T1 — remembered so the hover card's footer "usages"/"callers" action
    // hints (design-ui.md §9.1) can re-issue `gr`/`gc` against the SAME
    // cursor position that opened this card, without `PeekPanel` itself
    // needing to know anything about `WordPos`.
    lastHoverPosRef.current = { pane, pos };
    dispatchHier({ type: "CLOSE" });
    setPeekAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "OPEN", mode: "hover", word: pos.word });
    const reqId = ++peekReqRef.current;
    try {
      const resolved = await fetchResolve({ repo, path: panePath, line: pos.line, col: pos.col });
      if (reqId !== peekReqRef.current) return;
      const top = resolved.candidates[0];
      if (!top) {
        dispatchPeek({ type: "SET_ROWS", rows: [], approximate: false, note: resolved.note });
        return;
      }
      const card: HoverCard = { ident: resolved.ident, role: resolved.role, candidate: top };
      dispatchPeek({ type: "SET_CARD", card, note: resolved.note });

      let provenance: HoverProvenance;
      try {
        const why = await fetchWhyLine(top.repo, top.path, top.line);
        if (reqId !== peekReqRef.current) return;
        const confident = why.attribution.confidence === "trailer" || why.attribution.confidence === "exact";
        provenance = confident
          ? {
              none: false,
              displayName: why.attribution.display_name ?? why.attribution.session_id ?? null,
              commitSubject: why.region.subject,
            }
          : { none: true, displayName: null, commitSubject: null };
      } catch {
        if (reqId !== peekReqRef.current) return;
        provenance = { none: true, displayName: null, commitSubject: null };
      }
      dispatchPeek({ type: "SET_CARD_PROVENANCE", provenance });
    } catch (e) {
      if (reqId !== peekReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchPeek({ type: "SET_ERROR", message });
      toast.err(`K failed: ${message}`);
    }
  }

  function handlePeekActivate(row: PeekRow) {
    dispatchPeek({ type: "CLOSE" });
    const pane = peekOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (row.repo === repo && row.path === panePath) {
      // Same file already open — re-center without a navigation (mirrors
      // the outline rail's/annotations' own `jumpToLine` idiom).
      jumpToLine(pane, row.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, row.repo, row.path, row.line);
    }
  }

  function handlePeekClose() {
    dispatchPeek({ type: "CLOSE" });
    paneRepoPath(peekOwnerPaneRef.current).viewRef.current?.focus();
  }

  // --- V3.1-H3a — call/type hierarchy panels (gc / gC / gt) ---------------
  const [hier, dispatchHier] = useReducer(hierarchyReducer, initialHierarchyState);
  const [hierAnchor, setHierAnchor] = useState<PeekAnchor | null>(null);
  const hierOwnerPaneRef = useRef<1 | 2>(1);
  const hierReqRef = useRef(0);

  /**
   * Hierarchy endpoints require a callable def position. When the cursor is
   * on a call site / ref, resolve and use the top candidate's def location.
   */
  async function hierarchyPosWithResolveFallback(
    panePath: string,
    pos: WordPos,
  ): Promise<{ path: string; line: number; col: number; name: string }> {
    try {
      const resolved = await fetchResolve({
        repo,
        path: panePath,
        line: pos.line,
        col: pos.col,
      });
      const top = resolved.candidates[0];
      if (top && (resolved.role === "ref" || resolved.role === "import")) {
        return {
          path: top.path,
          line: top.line,
          col: 0,
          name: resolved.ident || pos.word,
        };
      }
      // On a def role (or unknown), try the cursor first.
      return {
        path: panePath,
        line: pos.line,
        col: pos.col,
        name: resolved.ident || pos.word,
      };
    } catch {
      return { path: panePath, line: pos.line, col: pos.col, name: pos.word };
    }
  }

  async function fetchCallersAt(
    path: string,
    line: number,
    col: number,
  ) {
    try {
      return await fetchHierarchyCallers({ repo, path, line, col });
    } catch (e) {
      // Cursor may have been on a call site without a prior resolve hit —
      // last resort: resolve then retry once.
      const resolved = await fetchResolve({ repo, path, line, col });
      const top = resolved.candidates[0];
      if (!top) throw e;
      return await fetchHierarchyCallers({
        repo,
        path: top.path,
        line: top.line,
        col: 0,
      });
    }
  }

  async function fetchCalleesAt(path: string, line: number, col: number) {
    try {
      return await fetchHierarchyCallees({ repo, path, line, col });
    } catch (e) {
      const resolved = await fetchResolve({ repo, path, line, col });
      const top = resolved.candidates[0];
      if (!top) throw e;
      return await fetchHierarchyCallees({
        repo,
        path: top.path,
        line: top.line,
        col: 0,
      });
    }
  }

  async function handleHierarchyCallers(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== hierReqRef.current) return;
    dispatchHier({ type: "OPEN", mode: "callers", title: target.name });
    try {
      const out = await fetchCallersAt(target.path, target.line, target.col);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildCallersTree(out) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gc failed: ${message}`);
    }
  }

  async function handleHierarchyCallees(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== hierReqRef.current) return;
    dispatchHier({ type: "OPEN", mode: "callees", title: target.name });
    try {
      const out = await fetchCalleesAt(target.path, target.line, target.col);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildCalleesTree(out) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gC failed: ${message}`);
    }
  }

  async function handleHierarchyTypes(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    hierOwnerPaneRef.current = pane;
    setHierAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    const reqId = ++hierReqRef.current;
    dispatchHier({ type: "OPEN", mode: "types", title: pos.word });
    try {
      // Resolve to learn kind — toast if not type-ish.
      let name = pos.word;
      let kind: string | null = null;
      try {
        const resolved = await fetchResolve({
          repo,
          path: panePath,
          line: pos.line,
          col: pos.col,
        });
        if (reqId !== hierReqRef.current) return;
        name = resolved.ident || pos.word;
        kind = resolved.candidates[0]?.kind ?? null;
      } catch {
        // Fall through with the word; types endpoint is name-based.
      }
      if (kind && !isTypeIshKind(kind)) {
        dispatchHier({ type: "CLOSE" });
        toast.err("not a type");
        return;
      }
      const out = await fetchHierarchyTypes(repo, name, panePath);
      if (reqId !== hierReqRef.current) return;
      dispatchHier({ type: "SET_TREE", roots: buildTypesTree(out, panePath) });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({ type: "SET_ERROR", message });
      toast.err(`gt failed: ${message}`);
    }
  }

  function handleHierActivate(node: HierarchyNode) {
    if (!node.path || node.line <= 0) return;
    dispatchHier({ type: "CLOSE" });
    const pane = hierOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (node.path === panePath) {
      jumpToLine(pane, node.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, node.path, node.line);
    }
  }

  function handleHierClose() {
    dispatchHier({ type: "CLOSE" });
    paneRepoPath(hierOwnerPaneRef.current).viewRef.current?.focus();
  }

  // --- V3.1-H3b — impact panel (`gi`) + ego-graph (`gG`) ------------------
  const [impact, dispatchImpact] = useReducer(impactReducer, initialImpactState);
  const [impactAnchor, setImpactAnchor] = useState<PeekAnchor | null>(null);
  const impactOwnerPaneRef = useRef<1 | 2>(1);
  const impactReqRef = useRef(0);

  type EgoUiState = {
    open: boolean;
    title: string;
    loading: boolean;
    error: string | null;
    layout: EgoLayoutResult | null;
    center: { path: string; line: number; col: number; name: string };
  };
  const [ego, setEgo] = useState<EgoUiState | null>(null);
  const egoOwnerPaneRef = useRef<1 | 2>(1);
  const egoReqRef = useRef(0);

  async function handleImpact(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    impactOwnerPaneRef.current = pane;
    setImpactAnchor(capturePeekAnchor(pane));
    dispatchPeek({ type: "CLOSE" });
    dispatchHier({ type: "CLOSE" });
    setEgo(null);
    const reqId = ++impactReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== impactReqRef.current) return;
    dispatchImpact({ type: "OPEN", title: target.name });
    try {
      const out = await fetchImpactAnalysis({
        repo,
        path: target.path,
        line: target.line,
        col: target.col || 0,
      });
      if (reqId !== impactReqRef.current) return;
      dispatchImpact({ type: "SET_DATA", data: out });
    } catch (e) {
      if (reqId !== impactReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchImpact({ type: "SET_ERROR", message });
      toast.err(`gi failed: ${message}`);
    }
  }

  function handleImpactActivate(row: ImpactFlatRow) {
    if (!isImpactNavigable(row)) return;
    dispatchImpact({ type: "CLOSE" });
    const pane = impactOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (row.path === panePath) {
      jumpToLine(pane, row.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, row.path, row.line);
    }
  }

  function handleImpactClose() {
    dispatchImpact({ type: "CLOSE" });
    paneRepoPath(impactOwnerPaneRef.current).viewRef.current?.focus();
  }

  async function buildEgoNeighborhood(
    path: string,
    line: number,
    col: number,
    name: string,
  ): Promise<{ layout: EgoLayoutResult; title: string }> {
    const centerId = `c:${path}:${line}`;
    const nodes: EgoLayoutInput["nodes"] = {};
    const inEdges: EgoLayoutInput["inEdges"] = [];
    const outEdges: EgoLayoutInput["outEdges"] = [];

    let centerName = name;
    try {
      const callers = await fetchCallersAt(path, line, col);
      centerName = callers.function.name || name;
      for (const g of callers.callers) {
        const site = g.sites[0];
        if (!site) continue;
        const id = `in:${g.path}:${g.enclosing?.line ?? site.line}`;
        nodes[id] = {
          id,
          name: g.enclosing?.name ?? g.path,
          class: site.class,
          path: g.path,
          line: g.enclosing?.line ?? site.line,
          kind: g.enclosing?.kind,
        };
        inEdges.push({ from: id, to: centerId, class: site.class });
      }
    } catch {
      // callers optional for types / non-callables
    }
    try {
      const callees = await fetchCalleesAt(path, line, col);
      centerName = callees.function.name || centerName;
      for (const s of callees.callees) {
        const id = `out:${s.target?.path ?? s.name}:${s.target?.line ?? s.line}`;
        nodes[id] = {
          id,
          name: s.name,
          class: s.class,
          path: s.target?.path,
          line: s.target?.line ?? s.line,
        };
        outEdges.push({ from: centerId, to: id, class: s.class });
      }
    } catch {
      // callees optional
    }
    // Types: also pull implementors as out-edges when name is type-ish.
    try {
      const types = await fetchHierarchyTypes(repo, centerName, path);
      for (const e of types.subtypes) {
        const id = `impl:${e.target?.path ?? e.via.path}:${e.target?.line ?? e.via.line}`;
        if (nodes[id]) continue;
        nodes[id] = {
          id,
          name: e.name,
          class: e.class,
          path: e.target?.path ?? e.via.path,
          line: e.target?.line ?? e.via.line,
          kind: e.kind,
        };
        outEdges.push({ from: centerId, to: id, class: e.class });
      }
    } catch {
      // ignore
    }

    const layout = layoutEgoGraph({
      center: {
        id: centerId,
        name: centerName,
        class: "exact",
        path,
        line,
      },
      inEdges,
      outEdges,
      nodes,
      depth: 1,
      nodeCap: 40,
    });
    return { layout, title: centerName };
  }

  async function handleEgoGraph(pane: 1 | 2, pos: WordPos) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    egoOwnerPaneRef.current = pane;
    dispatchPeek({ type: "CLOSE" });
    dispatchHier({ type: "CLOSE" });
    dispatchImpact({ type: "CLOSE" });
    const reqId = ++egoReqRef.current;
    const target = await hierarchyPosWithResolveFallback(panePath, pos);
    if (reqId !== egoReqRef.current) return;
    setEgo({
      open: true,
      title: target.name,
      loading: true,
      error: null,
      layout: null,
      center: target,
    });
    try {
      const { layout, title } = await buildEgoNeighborhood(
        target.path,
        target.line,
        target.col || 0,
        target.name,
      );
      if (reqId !== egoReqRef.current) return;
      setEgo({
        open: true,
        title,
        loading: false,
        error: null,
        layout,
        center: { ...target, name: title },
      });
    } catch (e) {
      if (reqId !== egoReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setEgo({
        open: true,
        title: target.name,
        loading: false,
        error: message,
        layout: null,
        center: target,
      });
      toast.err(`gG failed: ${message}`);
    }
  }

  function handleEgoActivate(node: EgoLaidOutNode) {
    if (!node.path || !node.line) return;
    setEgo(null);
    const pane = egoOwnerPaneRef.current;
    const { path: panePath, viewRef } = paneRepoPath(pane);
    if (node.path === panePath) {
      jumpToLine(pane, node.line);
      viewRef.current?.focus();
    } else {
      navigateToCandidate(pane, repo, node.path, node.line);
    }
  }

  async function handleEgoRecenter(node: EgoLaidOutNode) {
    if (!node.path || !node.line) return;
    const reqId = ++egoReqRef.current;
    setEgo((prev) =>
      prev
        ? {
            ...prev,
            loading: true,
            title: node.name,
            center: {
              path: node.path!,
              line: node.line!,
              col: 0,
              name: node.name,
            },
          }
        : prev,
    );
    try {
      const { layout, title } = await buildEgoNeighborhood(
        node.path,
        node.line,
        0,
        node.name,
      );
      if (reqId !== egoReqRef.current) return;
      setEgo({
        open: true,
        title,
        loading: false,
        error: null,
        layout,
        center: { path: node.path, line: node.line, col: 0, name: title },
      });
    } catch (e) {
      if (reqId !== egoReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      setEgo((prev) =>
        prev ? { ...prev, loading: false, error: message } : prev,
      );
    }
  }

  function handleEgoClose() {
    setEgo(null);
    paneRepoPath(egoOwnerPaneRef.current).viewRef.current?.focus();
  }

  function handleLensUsages(pane: 1 | 2, decl: LensDeclaration) {
    void handleFindRefs(pane, {
      line: decl.line,
      col: 0,
      word: decl.name,
    });
  }

  function handleLensImpls(pane: 1 | 2, decl: LensDeclaration) {
    void handleHierarchyTypes(pane, {
      line: decl.line,
      col: 0,
      word: decl.name,
    });
  }

  function handleLensAuthor(_pane: 1 | 2, decl: LensDeclaration) {
    inspectorRef.current?.openTab("provenance");
    // Best-effort: open why for the declaration line if gutter is on.
    if (provenanceMode !== "off") {
      handleBlameClick(decl.line);
    }
  }

  async function handleHierToggleExpand(node: HierarchyNode) {
    // Collapse if already expanded.
    if (node.expanded) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: false,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Section headers / already-loaded children: just expand.
    if (node.children.length > 0 || node.cycle || node.depthCapped || node.truncatedNote) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: true,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Root row in call modes already has children from first fetch.
    if (node.depth === 0) {
      const roots = updateNode(hier.roots, node.id, (n) => ({
        ...n,
        expanded: true,
      }));
      dispatchHier({ type: "PATCH_ROOTS", roots });
      return;
    }
    // Lazy fetch next level.
    if (node.depth >= HIERARCHY_DEPTH_CAP) return;
    const mode: HierarchyMode = hier.mode;
    const chain = collectAncestorLocs(hier.roots, node.id) ?? [];
    const ancestors = ancestorKeys(chain);

    dispatchHier({
      type: "PATCH_ROOTS",
      roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: true })),
    });

    const reqId = ++hierReqRef.current;
    try {
      let children: HierarchyNode[] = [];
      if (mode === "callers" || node.dir === "callers") {
        const out = await fetchHierarchyCallers({
          repo,
          path: node.path,
          line: node.line,
          col: node.col,
        });
        if (reqId !== hierReqRef.current) return;
        children = callersToNodes(out.callers, node.depth + 1, "callers", ancestors, out.truncated);
      } else if (mode === "callees" || node.dir === "callees") {
        const out = await fetchHierarchyCallees({
          repo,
          path: node.path,
          line: node.line,
          col: node.col,
        });
        if (reqId !== hierReqRef.current) return;
        children = calleesToNodes(out.callees, node.depth + 1, "callees", ancestors);
      } else {
        // Types: expand a type edge into its own supers/subs one level.
        const out = await fetchHierarchyTypes(repo, node.name, node.path || undefined);
        if (reqId !== hierReqRef.current) return;
        if (node.dir === "supertypes") {
          children = typeEdgesToNodes(
            out.supertypes,
            node.depth + 1,
            "supertypes",
            "supertypes",
            ancestors,
          );
        } else {
          children = typeEdgesToNodes(
            out.subtypes,
            node.depth + 1,
            "subtypes",
            "subtypes",
            ancestors,
          );
        }
      }
      if (reqId !== hierReqRef.current) return;
      dispatchHier({
        type: "PATCH_ROOTS",
        roots: updateNode(hier.roots, node.id, (n) => ({
          ...n,
          loading: false,
          expanded: true,
          children,
        })),
      });
    } catch (e) {
      if (reqId !== hierReqRef.current) return;
      const message = e instanceof Error ? e.message : String(e);
      dispatchHier({
        type: "PATCH_ROOTS",
        roots: updateNode(hier.roots, node.id, (n) => ({ ...n, loading: false })),
      });
      toast.err(`hierarchy expand: ${message}`);
    }
  }

  /// `lineEnd` selects the WHOLE `[line, lineEnd]` span (a `range`
  /// annotation's badge click, Phase D) rather than collapsing to `line`
  /// alone — `GotoSel`'s own `start`/`end` already support this; every
  /// other caller (outline jump, symbol/line annotation goto, peek
  /// activation) just omits it and gets the old single-line behavior.
  function jumpToLine(pane: 1 | 2, line: number, lineEnd?: number) {
    nonceRef.current += 1;
    const sel = { start: line, end: lineEnd ?? line, nonce: nonceRef.current };
    if (pane === 1) setGotoSel1(sel);
    else setGotoSel2(sel);
    // Same-file gd / peek / outline landings — still a jump.
    const p = pane === 1 ? activeFile : pane2Loc?.path;
    if (p) recordJump({ repo, path: p, line, snippet: "" });
  }

  // --- Wave E — the working set: `[f ]f` cycles the FOCUSED pane's file ---
  function handleCycleFile(pane: 1 | 2, dir: -1 | 1) {
    const current = pane === 1 ? activeFile : pane2Loc?.path;
    const next = workingSet.cycle(current, dir);
    if (!next || next === current) return;
    if (pane === 1) {
      navigate(buildReaderUrl({ pane1: { path: next, ref: gitRef } }));
    } else {
      navigate(buildReaderUrl({ pane2: { path: next, ref: pane2Loc?.ref } }));
    }
  }

  // --- Wave E — Ctrl-w v / Ctrl-w q: split-with-self / close the focused
  // pane ----------------------------------------------------------------
  /// `Ctrl-w v` — "split with self," the vim gesture: opens whichever
  /// file/ref `pane` currently shows into pane2 at the SAME cursor line,
  /// then focuses pane2 (a fresh `EditorView`'s auto-focus effect does the
  /// actual `.focus()` call once its data — already cached, same query key
  /// — resolves, typically on the very next tick).
  function handleSplitSelf(pane: 1 | 2) {
    const { path: panePath, ref: paneRef } = paneRepoPath(pane);
    if (!panePath) return;
    const line = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    navigate(buildReaderUrl({ pane2: { path: panePath, ref: paneRef, line } }));
    setFocusedPane(2);
  }

  /// `Ctrl-w q` — closes `pane` (a no-op if there's no second pane to begin
  /// with — nothing to close down TO). Closing pane2 just drops the query
  /// param; closing pane1 PROMOTES pane2's location into pane1's URL slot
  /// (the split collapses to a single view of what pane2 was showing).
  /// Either way exactly one pane remains, so focus always settles on it.
  function handleClosePane(pane: 1 | 2) {
    if (!pane2Loc) return;
    if (pane === 2) {
      navigate(buildReaderUrl({ pane2: null }));
    } else {
      navigate(buildReaderUrl({ pane1: { path: pane2Loc.path, ref: pane2Loc.ref, line: pane2Loc.line }, pane2: null }));
    }
    setFocusedPane(1);
    // Covers the "closing pane2, pane1's own content is unchanged" case
    // immediately (its own focus-follows-file effect won't re-fire since
    // `activeFile`/`gitRef` didn't change); the "promoting pane2's
    // DIFFERENT file into pane1" case is covered by that same effect once
    // the new blobHash lands, same as any other pane1 navigation.
    codeViewRef1.current?.focus();
  }

  function handlePaneFocus(pane: 1 | 2, dir: "prev" | "next") {
    if (pane === 1) {
      if (dir === "prev") {
        // Blur the buffer — the window-level handler owns tree keys again.
        (document.activeElement as HTMLElement | null)?.blur?.();
        setTreeVisible(true);
        return;
      }
      if (pane2Loc) codeViewRef2.current?.focus();
      // else: no pane2 to move to — a no-op (mirrors the pre-split
      // behavior, where this branch just refocused the buffer it was
      // already in).
      return;
    }
    // pane === 2
    if (dir === "prev") codeViewRef1.current?.focus();
    // dir === "next" from pane2: no pane3 — a dead end, no-op.
  }

  function vimCallbacksForPane(pane: 1 | 2): VimReaderCallbacks {
    return {
      onAnnotate: (sel) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        setAnnotationActiveLine(sel.line);
        // A visual-mode `a` carries `lineEnd` (`vimReader.ts`'s
        // `lineSelForCallback`) — previously dropped on the floor; Phase D's
        // composer needs it to offer a `range` annotation instead of always
        // collapsing to the selection's start line.
        setAnnotationActiveLineEnd(sel.lineEnd ?? null);
        inspectorRef.current?.openTab("annotations");
      },
      onPermalink: (sel) => handlePermalinkForPane(pane, sel),
      onGotoDef: (pos) => void handleGotoDef(pane, pos),
      onFindRefs: (pos) => void handleFindRefs(pane, pos),
      onHover: (pos) => void handleHover(pane, pos),
      onHistoryStep: (dir) => handleHistoryStep(pane, dir),
      onCycleFile: (dir) => handleCycleFile(pane, dir),
      onPaneFocus: (dir) => handlePaneFocus(pane, dir),
      onSplitSelf: () => handleSplitSelf(pane),
      onClosePane: () => handleClosePane(pane),
      onShowHelp: () => setHelpOpen(true),
      // V3.N1 — nav memory. `onRecordJump` is the sink for G/gg/mark/
      // search-step records from vimReader; Ctrl-o/i + g. are host-owned.
      onRecordJump: (info) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        recordJump({ repo, path: panePath, line: info.line, snippet: info.snippet });
      },
      onJumpBack: () => handleJumpBack(),
      onJumpForward: () => handleJumpForward(),
      onRecentLocations: () => setRecentLocsOpen(true),
      onStructurePopup: () => setStructureOpen(true),
      onToggleBookmark: () => void handleToggleBookmark(pane),
      onMnemonicPopup: () => setMnemonicOpen(true),
      onLineHistory: (sel) => {
        const { path: panePath } = paneRepoPath(pane);
        if (!panePath) return;
        setLineHistory({ path: panePath, line: sel.line, lineEnd: sel.lineEnd });
      },
      onHierarchyCallers: (pos) => void handleHierarchyCallers(pane, pos),
      onHierarchyCallees: (pos) => void handleHierarchyCallees(pane, pos),
      onHierarchyTypes: (pos) => void handleHierarchyTypes(pane, pos),
      onImpact: (pos) => void handleImpact(pane, pos),
      onEgoGraph: (pos) => void handleEgoGraph(pane, pos),
    };
  }

  async function handleToggleBookmark(pane: 1 | 2) {
    const { path: panePath } = paneRepoPath(pane);
    if (!panePath) return;
    const line = pane === 1 ? cursorLineRef1.current : cursorLineRef2.current;
    const existing = (bookmarksQ.data?.bookmarks ?? []).find(
      (b) => b.path === panePath && b.line === line,
    );
    try {
      if (existing) {
        await deleteBookmarkMut.mutateAsync(existing.id);
        toast.ok(`bookmark removed · ${panePath}:${line}`);
      } else {
        await createBookmarkMut.mutateAsync({ repo, path: panePath, line });
        toast.ok(`bookmark added · ${panePath}:${line}`);
      }
    } catch (e) {
      toast.err(`bookmark: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  function handleJumpBookmark(loc: { path: string; line: number }) {
    recordJump({ repo, path: loc.path, line: loc.line, snippet: "" });
    if (loc.path !== activeFile) {
      skipNavRecordRef.current = true;
    }
    navigate(buildReaderUrl({ pane1: { path: loc.path, ref: gitRef, line: loc.line } }));
    setFocusedPane(1);
  }

  // --- B1 — linkify: paths/URLs/session-ids inside comments+strings --------
  // Wave E: a path/sha clicked INSIDE a given pane opens into THAT pane
  // (keeps the interaction local to where the click happened), preserving
  // whatever the other pane is showing.
  function linkifyCallbacksForPane(pane: 1 | 2): LinkifyCallbacks {
    return {
      onOpenUrl: (url) => window.open(url, "_blank", "noopener,noreferrer"),
      onOpenPath: (linkedPath) => {
        const { ref: paneRef } = paneRepoPath(pane);
        if (pane === 1) navigate(buildReaderUrl({ pane1: { path: linkedPath, ref: paneRef } }));
        else navigate(buildReaderUrl({ pane2: { path: linkedPath, ref: paneRef } }));
      },
      onOpenSession: (id) => window.open(sessionUrl(id), "_blank", "noopener,noreferrer"),
      // Wave C — a bare commit sha in a comment/string now has a real
      // destination (the commit page hub) — always a full navigation away
      // from the reader entirely, so this intentionally does NOT preserve
      // pane2 (a commit page has no file pane to carry it into).
      onOpenSha: (sha) => navigate(commitUrl(repo, sha)),
    };
  }

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target)) return;
      if (isInsideBuffer(e.target)) return; // vimReader owns the buffer's keys
      switch (e.key) {
        case "j":
          e.preventDefault();
          treeRef.current?.moveFocus(1);
          break;
        case "k":
          e.preventDefault();
          treeRef.current?.moveFocus(-1);
          break;
        case "Enter":
          // Wave E — Shift+Enter opens the focused tree row into pane2
          // regardless of which pane currently has focus; plain Enter
          // keeps going to whichever pane IS focused (FileTree's own
          // default when no target is given).
          treeRef.current?.activateFocused(e.shiftKey ? "pane2" : undefined);
          break;
        case "/":
          e.preventDefault();
          setTreeVisible(true);
          treeRef.current?.focusFilter();
          break;
        case "b":
          setTreeVisible((v) => !v);
          break;
        case "a":
          if (isFile && !diffMode && !storyMode) {
            e.preventDefault();
            setAnnotationActiveLine(cursorLineRef1.current);
            setAnnotationActiveLineEnd(null);
            inspectorRef.current?.openTab("annotations");
          }
          break;
        case "?":
          e.preventDefault();
          setHelpOpen(true);
          break;
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [isFile, diffMode, storyMode]);

  // F5 — Esc closes the mobile reader-tools sheet (mirrors `MobileDrawer`'s
  // own Esc handling for the tree drawer). Skips typing targets + anything
  // inside a CM6 buffer (vim's own Escape — e.g. leaving visual mode — must
  // never ALSO close the sheet) and modifier chords; only mounts while the
  // sheet is actually open, same "one small effect scoped to `open`" idiom
  // kb's own `detail.tsx` uses for its equivalent reader-tools sheet.
  useEffect(() => {
    if (!inspectorOpen) return;
    function onKey(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      if (isEditableTarget(e.target) || isInsideBuffer(e.target)) return;
      e.preventDefault();
      setInspectorOpen(false);
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [inspectorOpen]);

  function handleSelect(selectedPath: string, kind: EntryKind, target?: "focused" | "pane2") {
    if (kind !== "file") return;
    openPath(selectedPath, target ?? "focused");
    // F5 — on mobile the tree is an overlay drawer; picking a file should
    // return the operator straight to reading it, not leave the drawer
    // covering the screen over the file they just opened.
    if (isMobile) setTreeVisible(false);
  }

  const focusedFileData = focusedPane === 1 ? file.data : pane2File.data;
  const focusedSymbols = focusedFileData && focusedFileData.encoding === "utf8" ? focusedFileData.symbols : [];
  // F5 — mobile-only reader-tools sheet is available whenever the outline
  // aside itself would render (see the aside's own condition below); kept as
  // a plain boolean so both the header's entry button and the aside's render
  // condition read the identical gate.
  const hasInspector = !diffMode && !storyMode && !!file.data && file.data.encoding === "utf8";
  // F5 — CSS-only sheet promotion (mirrors kb's own `.detail--inspector-open`
  // / `.kb-pinsp`, root CLAUDE.md invariant #30): gated on `isMobile` so a
  // stray `inspectorOpen=true` left over from a mobile session never adds
  // this modifier after a resize back to desktop — desktop DOM/CSS stays
  // untouched regardless of this flag's value.
  const readerRootClass = "kbc-reader" + (isMobile && inspectorOpen ? " kbc-reader--sheet-open" : "");

  return (
    <div className={readerRootClass}>
      <header className="kbc-reader__top">
        {/* F5 — mobile-only hamburger (CSS-hidden ≥861px): promotes the tree
            aside to an overlay `MobileDrawer` below. Reuses `Icon.List`'s
            3-line glyph, which already reads as a hamburger — see
            `icons.tsx`'s own doc on why no separate Menu icon was added. */}
        <button
          type="button"
          className="kbc-burger"
          onClick={() => setTreeVisible((v) => !v)}
          aria-label={treeVisible ? "close file tree" : "open file tree"}
          aria-expanded={treeVisible}
          data-kbc-burger
        >
          <Icon.List />
        </button>
        <Breadcrumbs
          repo={repo}
          path={path}
          gitRef={gitRef}
          symbols={focusedSymbols}
          cursorLine={cursorLineUi}
          onJumpSymbol={(line) => jumpToLine(focusedPane, line)}
        />
        {/* F1 — the search launcher moved to the global TopBar (app.tsx),
            mounted once above every route; this header no longer duplicates
            it. */}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (stickyEnabled ? " is-on" : "")}
            aria-pressed={stickyEnabled}
            title="Sticky context lines"
            data-kbc-sticky-toggle
            onClick={() => {
              setStickyEnabled((v) => {
                const next = !v;
                saveStickyContext(next);
                return next;
              });
            }}
          >
            Sticky
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (paramHintsEnabled ? " is-on" : "")}
            aria-pressed={paramHintsEnabled}
            title="Param-name inlay hints at call sites"
            data-kbc-param-hints-toggle
            onClick={() => {
              setParamHintsEnabled((v) => {
                const next = !v;
                saveParamHints(next);
                return next;
              });
            }}
          >
            Params
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (codeLensesEnabled ? " is-on" : "")}
            aria-pressed={codeLensesEnabled}
            title="Code Vision lens chips above declarations"
            data-kbc-lenses-toggle
            onClick={() => {
              setCodeLensesEnabled((v) => {
                const next = !v;
                saveCodeLenses(next);
                return next;
              });
            }}
          >
            Lenses
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <button
            type="button"
            className={"kbc-sticky-toggle" + (wrapEnabled ? " is-on" : "")}
            aria-pressed={wrapEnabled}
            title="Wrap long lines"
            data-kbc-wrap-toggle
            onClick={() => {
              setWrapEnabled((v) => {
                const next = !v;
                saveWrap(next);
                return next;
              });
            }}
          >
            Wrap
          </button>
        )}
        {isFile && !diffMode && !storyMode && (
          <div className="kbc-fontsize-stepper" role="group" aria-label="Reader font size" data-kbc-fontsize-stepper>
            <button
              type="button"
              className="kbc-fontsize-stepper__btn"
              aria-label="Decrease reader font size"
              title="Smaller text"
              data-kbc-fontsize-dec
              disabled={readerFontSize <= READER_FONT_SIZE_MIN}
              onClick={() => setReaderFontSize((cur) => saveReaderFontSize(cur - 1))}
            >
              A−
            </button>
            <button
              type="button"
              className="kbc-fontsize-stepper__btn"
              aria-label="Increase reader font size"
              title="Larger text"
              data-kbc-fontsize-inc
              disabled={readerFontSize >= READER_FONT_SIZE_MAX}
              onClick={() => setReaderFontSize((cur) => saveReaderFontSize(cur + 1))}
            >
              A+
            </button>
          </div>
        )}
        {isFile && !diffMode && !storyMode && (
          <div className="kbc-provenance-toggle" role="group" aria-label="Provenance overlay">
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "off" ? " is-active" : "")}
              aria-pressed={provenanceMode === "off"}
              onClick={() => setProvenanceMode("off")}
              data-kbc-provenance-mode="off"
            >
              Off
            </button>
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "dots" ? " is-active" : "")}
              aria-pressed={provenanceMode === "dots"}
              onClick={() => setProvenanceMode((m) => (m === "dots" ? "off" : "dots"))}
              data-kbc-provenance-toggle
              data-kbc-provenance-mode="dots"
              title="Show blame provenance dots in the gutter"
            >
              Dots
            </button>
            <button
              type="button"
              className={"kbc-provenance-toggle__opt" + (provenanceMode === "age" ? " is-active" : "")}
              aria-pressed={provenanceMode === "age"}
              onClick={() => setProvenanceMode((m) => (m === "age" ? "off" : "age"))}
              data-kbc-provenance-mode="age"
              title="Tint lines by author age"
            >
              Age
            </button>
          </div>
        )}
        <RefPicker repo={repo} path={path} activeRef={gitRef} pane2={pane2Loc ?? undefined} />
        {/* Phase E4 — capture the FOCUSED pane's open file (or its current
            selection, when one exists) into a reading set. */}
        {focusedPath !== undefined && !diffMode && !storyMode && (
          <AddToSetMenu repo={repo} path={focusedPath} getSelection={currentPaneSelection} />
        )}
        {/* F5 — mobile-only "reader tools" sheet entry button (CSS-hidden
            ≥861px). Badged with the unresolved-annotation count so an
            operator knows there's something to look at before opening it. */}
        {hasInspector && (
          <button
            type="button"
            className="kbc-inspector-toggle"
            onClick={() => setInspectorOpen((v) => !v)}
            aria-label="reader tools"
            aria-controls="kbc-reader-sheet"
            aria-expanded={inspectorOpen}
            data-kbc-inspector-toggle
          >
            <Icon.Panel />
            {unresolvedAnnotationsCount > 0 && (
              <span className="kbc-inspector-toggle__badge" data-kbc-inspector-toggle-badge>
                {unresolvedAnnotationsCount}
              </span>
            )}
          </button>
        )}
      </header>
      <WorkingSetStrip
        entries={workingSet.entries}
        pane1Path={activeFile}
        pane2Path={pane2Loc?.path}
        onOpen={openPath}
        onPin={workingSet.pin}
        onUnpin={workingSet.unpin}
        onRemove={workingSet.remove}
      />
      <RepoStateBanner repo={repo} />
      {liveMirror1.headMoved && (
        <HeadMovedBanner newRef={liveMirror1.headMoved.new} onRefreshTree={refreshTree} onDismiss={liveMirror1.dismissHeadMoved} />
      )}
      <div className="kbc-reader__body">
        {/* F5 — on mobile the tree is promoted to an overlay `MobileDrawer`
            (focus-trap + Esc + scrim + body-scroll-lock all come from that
            ported component, mirroring kb's own nav/filter drawer); desktop
            keeps the exact pre-F5 inline `<aside>` markup. Either way there's
            only ONE `FileTree` instance mounted at a time — `treeRef` never
            needs to pick between two live copies. */}
        {isMobile ? (
          <MobileDrawer
            open={treeVisible}
            onClose={() => setTreeVisible(false)}
            title="Files"
            ariaLabel="file tree"
          >
            <FileTree ref={treeRef} repo={repo} gitRef={gitRef} selectedPath={path} onSelect={handleSelect} />
          </MobileDrawer>
        ) : (
          treeVisible && (
            <aside className="kbc-reader__tree" data-region="tree">
              {/* F1 — the repo switcher moved to the global RepoPill (TopBar,
                  app.tsx); the sidebar is file-tree-only now. */}
              <FileTree ref={treeRef} repo={repo} gitRef={gitRef} selectedPath={path} onSelect={handleSelect} />
            </aside>
          )
        )}
        <main className="kbc-reader__main" id="main" data-region="main">
          {diffMode ? (
            fromRef ? (
              <DiffView repo={repo} path={path} from={fromRef} to={toRef} />
            ) : (
              <div className="kbc-reader__hint">Missing ?from= for the diff view</div>
            )
          ) : storyMode ? (
            isFile ? (
              <StoryPlayer
                repo={repo}
                path={path}
                atSha={atRef}
                onExit={(sha) => navigate(codeUrl({ repo, path, ref: sha }))}
              />
            ) : (
              <div className="kbc-reader__hint">
                Story mode needs a file — open one from the tree, then use the History tab's Story button.
              </div>
            )
          ) : historySentinelMode ? (
            <div className="kbc-reader__hint">
              This link is missing required parameters — use the History tab, or the Compare/Branches pages, to get
              here.
            </div>
          ) : (
            <div className="kbc-reader__panes">
              <div
                className={"kbc-reader__pane" + (pane2Loc && focusedPane === 1 ? " is-focused" : "")}
                onFocus={() => setFocusedPane(1)}
                data-region="pane-1"
              >
                {liveMirror1.fileChangedOnDisk && (
                  <FileChangedToast
                    paneLabel={fileChangedPaneLabel(1, !!pane2Loc)}
                    onRefresh={() => {
                      void file.refetch();
                      liveMirror1.dismissFileChanged();
                    }}
                    onDismiss={liveMirror1.dismissFileChanged}
                  />
                )}
                {file.isLoading ? (
                  <div className="kbc-reader__hint">Loading…</div>
                ) : file.error ? (
                  <div className="kbc-reader__hint kbc-reader__hint--error">{(file.error as Error).message}</div>
                ) : file.data ? (
                  file.data.encoding === "base64" ? (
                    <div className="kbc-reader__hint">Binary file — preview not supported</div>
                  ) : (
                    <CodeView
                      ref={codeViewRef1}
                      content={file.data.content}
                      spans={file.data.highlights}
                      blobHash={file.data.blob_hash}
                      gotoSel={gotoSel1}
                      vim={vimCallbacksForPane(1)}
                      onSelectionLines={(sel) => {
                        lastSelRef1.current = sel;
                        if (focusedPane === 1) cursorSyncRef1.current?.onSelection(sel);
                      }}
                      onVimStatus={setVimStat}
                      blameDots={focusedPane === 1 ? blameDots : null}
                      onBlameHover={focusedPane === 1 ? handleBlameHover : undefined}
                      onBlameUnhover={focusedPane === 1 ? handleBlameUnhover : undefined}
                      onBlameClick={focusedPane === 1 ? handleBlameClick : undefined}
                      ageLines={focusedPane === 1 ? ageLines : null}
                      annotationMarkers={focusedPane === 1 ? annotationMarkers : null}
                      onAnnotationClick={focusedPane === 1 ? handleAnnotationClick : undefined}
                      diagnosticMarkers={focusedPane === 1 ? diagMarks : null}
                      onViewerDirtyChange={(dirty) => {
                        viewerDirtyRef1.current = dirty;
                      }}
                      onCursorLineChange={(line) => {
                        cursorLineRef1.current = line;
                        if (focusedPane === 1) setCursorLineUi(line);
                      }}
                      symbols={file.data.symbols}
                      stickyContextEnabled={stickyEnabled}
                      onStickyJump={(line) => jumpToLine(1, line)}
                      paramHintsRepo={repo}
                      paramHintsPath={activeFile}
                      paramHintsEnabled={paramHintsEnabled}
                      lensDeclarations={lenses1.data?.declarations ?? null}
                      codeLensesEnabled={codeLensesEnabled}
                      onLensUsages={(d) => handleLensUsages(1, d)}
                      onLensImpls={(d) => handleLensImpls(1, d)}
                      onLensAuthor={(d) => handleLensAuthor(1, d)}
                      linkify={linkifyCallbacksForPane(1)}
                      conflictActive={!!activeFile && conflictedPaths.has(activeFile)}
                      wrap={wrapEnabled}
                      fontSize={readerFontSize}
                    />
                  )
                ) : (
                  <div className="kbc-reader-start">
                    <EmptyState
                      icon={<Icon.List />}
                      title="Select a file from the tree"
                      hint="Or press / to filter it."
                      action={{
                        label: "Search everywhere (⌘K)",
                        onClick: () => window.dispatchEvent(new CustomEvent("kbc:omnibox.open")),
                      }}
                    />
                    <ReaderStartCards repo={repo} onOpen={(p) => openPath(p, "focused")} />
                  </div>
                )}
              </div>
              {pane2Loc && (
                <>
                  <div className="kbc-reader__pane-rule" />
                  <div
                    className={"kbc-reader__pane" + (focusedPane === 2 ? " is-focused" : "")}
                    onFocus={() => setFocusedPane(2)}
                    data-region="pane-2"
                  >
                    {liveMirror2.fileChangedOnDisk && (
                      <FileChangedToast
                        paneLabel={fileChangedPaneLabel(2, true)}
                        onRefresh={() => {
                          void pane2File.refetch();
                          liveMirror2.dismissFileChanged();
                        }}
                        onDismiss={liveMirror2.dismissFileChanged}
                      />
                    )}
                    {pane2File.isLoading ? (
                      <div className="kbc-reader__hint">Loading…</div>
                    ) : pane2File.error ? (
                      <div className="kbc-reader__hint kbc-reader__hint--error">
                        {(pane2File.error as Error).message}
                      </div>
                    ) : pane2File.data ? (
                      pane2File.data.encoding === "base64" ? (
                        <div className="kbc-reader__hint">Binary file — preview not supported</div>
                      ) : (
                        <CodeView
                          ref={codeViewRef2}
                          content={pane2File.data.content}
                          spans={pane2File.data.highlights}
                          blobHash={pane2File.data.blob_hash}
                          gotoSel={gotoSel2}
                          vim={vimCallbacksForPane(2)}
                          onSelectionLines={(sel) => {
                            lastSelRef2.current = sel;
                            if (focusedPane === 2) cursorSyncRef2.current?.onSelection(sel);
                          }}
                          onVimStatus={setVimStat}
                          blameDots={focusedPane === 2 ? blameDots : null}
                          onBlameHover={focusedPane === 2 ? handleBlameHover : undefined}
                          onBlameUnhover={focusedPane === 2 ? handleBlameUnhover : undefined}
                          onBlameClick={focusedPane === 2 ? handleBlameClick : undefined}
                          ageLines={focusedPane === 2 ? ageLines : null}
                          annotationMarkers={focusedPane === 2 ? annotationMarkers : null}
                          onAnnotationClick={focusedPane === 2 ? handleAnnotationClick : undefined}
                          diagnosticMarkers={focusedPane === 2 ? diagMarks : null}
                          onViewerDirtyChange={(dirty) => {
                            viewerDirtyRef2.current = dirty;
                          }}
                          onCursorLineChange={(line) => {
                            cursorLineRef2.current = line;
                            if (focusedPane === 2) setCursorLineUi(line);
                          }}
                          symbols={pane2File.data.symbols}
                          stickyContextEnabled={stickyEnabled}
                          onStickyJump={(line) => jumpToLine(2, line)}
                          paramHintsRepo={repo}
                          paramHintsPath={pane2Loc?.path ?? null}
                          paramHintsEnabled={paramHintsEnabled}
                          lensDeclarations={lenses2.data?.declarations ?? null}
                          codeLensesEnabled={codeLensesEnabled}
                          onLensUsages={(d) => handleLensUsages(2, d)}
                          onLensImpls={(d) => handleLensImpls(2, d)}
                          onLensAuthor={(d) => handleLensAuthor(2, d)}
                          linkify={linkifyCallbacksForPane(2)}
                          conflictActive={!!pane2Loc?.path && conflictedPaths.has(pane2Loc.path)}
                          wrap={wrapEnabled}
                          fontSize={readerFontSize}
                        />
                      )
                    ) : null}
                  </div>
                </>
              )}
            </div>
          )}
          {peek.open && (
            <PeekPanel
              state={peek}
              currentRepo={repo}
              anchor={peekAnchor}
              onMove={(delta) => dispatchPeek({ type: "MOVE", delta })}
              onActivate={handlePeekActivate}
              onClose={handlePeekClose}
              // T1 — the hover card's "usages"/"callers" footer hints
              // (design-ui.md §9.1) reissue gr/gc against the position `K`
              // was pressed at; absent until a hover has actually happened.
              onFindRefs={
                lastHoverPosRef.current
                  ? () => void handleFindRefs(lastHoverPosRef.current!.pane, lastHoverPosRef.current!.pos)
                  : undefined
              }
              onFindCallers={
                lastHoverPosRef.current
                  ? () => void handleHierarchyCallers(lastHoverPosRef.current!.pane, lastHoverPosRef.current!.pos)
                  : undefined
              }
            />
          )}
          {hier.open && (
            <HierarchyPanel
              state={hier}
              currentRepo={repo}
              anchor={hierAnchor}
              onMove={(delta) => dispatchHier({ type: "MOVE", delta })}
              onActivate={handleHierActivate}
              onToggleExpand={(node) => void handleHierToggleExpand(node)}
              onClose={handleHierClose}
            />
          )}
          {impact.open && (
            <ImpactPanel
              state={impact}
              currentRepo={repo}
              anchor={impactAnchor}
              onMove={(delta) => dispatchImpact({ type: "MOVE", delta })}
              onActivate={handleImpactActivate}
              onToggleBucket={(bucket) => dispatchImpact({ type: "TOGGLE_BUCKET", bucket })}
              onClose={handleImpactClose}
            />
          )}
          {ego?.open && (
            <EgoGraph
              layout={
                ego.layout ?? {
                  nodes: [],
                  edges: [],
                  truncated: 0,
                  width: 200,
                  height: 100,
                }
              }
              title={ego.title}
              loading={ego.loading}
              error={ego.error}
              onActivate={handleEgoActivate}
              onRecenter={(n) => void handleEgoRecenter(n)}
              onClose={handleEgoClose}
            />
          )}
          {hoverChip && (
            <BlameChip rect={hoverChip.rect} label={hoverChip.label} solid={hoverChip.solid} />
          )}
          {copiedTick > 0 && (
            <div className="kbc-copied-chip" role="status" data-kbc-copied>
              Permalink copied
            </div>
          )}
          {(vimStat.mode !== "normal" || vimStat.pending !== "") && (
            <div className="kbc-vim-status" aria-live="polite" data-kbc-vim-status>
              {vimStat.mode === "visual-line" ? "V-LINE" : vimStat.mode === "visual" ? "VISUAL" : ""}
              {vimStat.pending !== "" && <span className="kbc-vim-status__pending">{vimStat.pending}</span>}
            </div>
          )}
        </main>
        {hasInspector && (
          <aside className="kbc-reader__outline" data-region="rail">
            <InspectorRail
              ref={inspectorRef}
              symbols={focusedSymbols}
              onJumpOutline={(line) => jumpToLine(focusedPane, line)}
              whyPanel={whyPanel}
              historyPanel={historyPanel}
              citedBy={citedBy}
              frameworkCard={frameworkCard}
              diagnosticsCard={diagnosticsCard}
              repo={repo}
              path={focusedPath ?? ""}
              annotationActiveLine={annotationActiveLine}
              annotationActiveLineEnd={annotationActiveLineEnd}
              bookmarkCount={bookmarkCount}
              onJumpBookmark={handleJumpBookmark}
              onGotoAnnotationLine={(line, lineEnd) => {
                jumpToLine(focusedPane, line, lineEnd);
                // Without this, the view never regains DOM focus (the
                // click originated in the inspector rail, not the
                // buffer) — CM6's own native-selection reconciliation can
                // then clobber the just-dispatched range/line a tick
                // later. Same "jumpToLine + explicit focus" idiom
                // `handleGotoDef`/`handlePeekActivate` already use.
                paneRepoPath(focusedPane).viewRef.current?.focus();
              }}
              unresolvedAnnotations={unresolvedAnnotationsCount}
              // F5 — `false`/`undefined` on desktop (isMobile is always
              // false there), so this `<aside>`'s markup is byte-identical
              // to pre-F5: no `asSheet` gate exercised, no sheet-head, no
              // `role="dialog"`.
              asSheet={isMobile}
              onMobileClose={isMobile ? () => setInspectorOpen(false) : undefined}
              entityPanel={
                focusedPath ? (
                  <EntityRail
                    repo={repo}
                    path={focusedPath}
                    cursorLine={cursorLineUi}
                    symbols={focusedSymbols}
                    lenses={
                      (focusedPane === 1 ? lenses1.data?.declarations : lenses2.data?.declarations) ??
                      null
                    }
                    onOpenGraph={(pos) =>
                      void handleEgoGraph(focusedPane, {
                        line: pos.line,
                        col: pos.col,
                        word: pos.word,
                      })
                    }
                    onOpenImpact={(pos) =>
                      void handleImpact(focusedPane, {
                        line: pos.line,
                        col: pos.col,
                        word: pos.word,
                      })
                    }
                    onJump={(line) => {
                      jumpToLine(focusedPane, line);
                      paneRepoPath(focusedPane).viewRef.current?.focus();
                    }}
                  />
                ) : null
              }
            />
          </aside>
        )}
      </div>
      {/* F5 — mobile scrim under the reader-tools sheet (mirrors the tree
          drawer's own `MobileDrawer`-owned scrim); tap to dismiss. Mobile-only
          so the desktop DOM is unchanged. */}
      {isMobile && inspectorOpen && (
        <div
          className="kbc-sheet-scrim is-open"
          onClick={() => setInspectorOpen(false)}
          aria-hidden
          data-kbc-sheet-scrim
        />
      )}
      <KeyboardHelp open={helpOpen} onClose={() => setHelpOpen(false)} context="reader" />
      <RecentLocations
        open={recentLocsOpen}
        onClose={() => {
          setRecentLocsOpen(false);
          // Return focus to the buffer the popup was opened from.
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        activeRepo={repo}
        onJump={(loc) => {
          // Picking from the popup is itself a navigation the operator
          // initiated — record it (unlike Ctrl-o/i which only move the
          // pointer). Then land. Suppress the file-open effect only when
          // the path actually changes (same-file jumps never re-fire it).
          recordJump({ repo: loc.repo, path: loc.path, line: loc.line, snippet: "" });
          if (loc.repo !== repo || loc.path !== activeFile) {
            skipNavRecordRef.current = true;
          }
          if (loc.repo !== repo) {
            navigate(codeUrl({ repo: loc.repo, path: loc.path, line: loc.line }));
          } else {
            navigate(buildReaderUrl({ pane1: { path: loc.path, ref: gitRef, line: loc.line } }));
            setFocusedPane(1);
          }
        }}
      />
      <StructurePopup
        open={structureOpen}
        onClose={() => {
          setStructureOpen(false);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        symbols={focusedSymbols}
        onJump={(line) => jumpToLine(focusedPane, line)}
        // T1 (design-ui.md §9.2) — copy-symbol-link coordinates for the
        // CURRENTLY FOCUSED pane's file.
        repo={repo}
        path={focusedPath}
        lang={focusedFileData?.lang}
      />
      <MnemonicPopup
        open={mnemonicOpen}
        onClose={() => {
          setMnemonicOpen(false);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        repo={repo}
        onJump={handleJumpBookmark}
      />
      <LineHistoryPopup
        open={lineHistory !== null}
        onClose={() => {
          setLineHistory(null);
          (focusedPane === 2 ? codeViewRef2 : codeViewRef1).current?.focus();
        }}
        repo={repo}
        path={lineHistory?.path ?? focusedPath ?? ""}
        line={lineHistory?.line ?? 1}
        lineEnd={lineHistory?.lineEnd}
      />
    </div>
  );
}
